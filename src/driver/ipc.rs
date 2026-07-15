use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub(super) const IO_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) enum FrameRead {
    Complete(Vec<u8>),
    TooLarge,
    Truncated,
    TimedOut,
}

pub(super) fn connect_with_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let path_bytes = path.as_os_str().as_bytes();
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if path_bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "driver IPC socket path is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr().cast::<libc::c_char>(),
            address.sun_path.as_mut_ptr(),
            path_bytes.len(),
        );
    }
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let address_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
        as libc::socklen_t;
    let connected = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::addr_of!(address).cast::<libc::sockaddr>(),
            address_len,
        )
    };
    if connected != 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(code) if code == libc::EINPROGRESS || code == libc::EAGAIN
        ) {
            return Err(error);
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(std::ptr::addr_of_mut!(poll), 1, timeout_ms) };
        if ready == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "driver IPC connect deadline exceeded",
            ));
        }
        if ready < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut socket_error = 0;
        let mut socket_error_len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                std::ptr::addr_of_mut!(socket_error).cast(),
                std::ptr::addr_of_mut!(socket_error_len),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if socket_error != 0 {
            return Err(io::Error::from_raw_os_error(socket_error));
        }
    }
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
}

pub(super) fn read_frame(stream: &mut UnixStream) -> io::Result<FrameRead> {
    read_frame_with_timeout(stream, IO_TIMEOUT)
}

pub(super) fn read_frame_with_timeout(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<FrameRead> {
    let mut reader = BufReader::new(stream);
    let mut frame = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let Some(remaining) = remaining_time(deadline) else {
            return Ok(FrameRead::TimedOut);
        };
        reader.get_mut().set_read_timeout(Some(remaining))?;
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Ok(FrameRead::TimedOut);
            }
            Err(err) => return Err(err),
        };
        if available.is_empty() {
            return Ok(FrameRead::Truncated);
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if frame.len().saturating_add(newline) > MAX_FRAME_BYTES {
                return Ok(FrameRead::TooLarge);
            }
            frame.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            return Ok(FrameRead::Complete(frame));
        }
        if frame.len().saturating_add(available.len()) > MAX_FRAME_BYTES {
            return Ok(FrameRead::TooLarge);
        }
        let consumed = available.len();
        frame.extend_from_slice(available);
        reader.consume(consumed);
    }
}

pub(super) fn write_frame(stream: &mut UnixStream, value: &serde_json::Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bytes = serde_json::to_vec(&serde_json::json!({
            "id": null,
            "ok": false,
            "error": {
                "code": "response_too_large",
                "message": "driver IPC response exceeds the maximum frame size"
            }
        }))
        .map_err(io::Error::other)?;
    }
    bytes.push(b'\n');
    write_bytes(stream, &bytes)
}

pub(super) fn write_bytes(stream: &mut UnixStream, mut bytes: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + IO_TIMEOUT;
    while !bytes.is_empty() {
        let Some(remaining) = remaining_time(deadline) else {
            return Err(write_timeout());
        };
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "driver IPC closed",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(write_timeout());
            }
            Err(err) => return Err(err),
        }
    }
    let Some(remaining) = remaining_time(deadline) else {
        return Err(write_timeout());
    };
    stream.set_write_timeout(Some(remaining))?;
    stream.flush().map_err(|err| {
        if matches!(
            err.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            write_timeout()
        } else {
            err
        }
    })
}

fn remaining_time(deadline: Instant) -> Option<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    (!remaining.is_zero()).then_some(remaining)
}

fn write_timeout() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "driver IPC write deadline exceeded",
    )
}
