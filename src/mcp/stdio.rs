use std::io::{self, BufRead, Read, Write};

#[cfg(unix)]
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::atomic::{AtomicI32, Ordering};
#[cfg(unix)]
use std::sync::{Mutex, MutexGuard};

use crate::adapters::tmux::CommandRunner;
use serde_json::Value;

use super::{McpServer, error_response};

pub fn serve_stdio<Reader, Writer>(reader: &mut Reader, writer: &mut Writer) -> io::Result<()>
where
    Reader: BufRead,
    Writer: Write,
{
    let mut server = McpServer::from_environment()?;
    serve_stdio_with_server(&mut server, reader, writer)
}

pub fn serve_stdio_with_server<R, Reader, Writer>(
    server: &mut McpServer<R>,
    reader: &mut Reader,
    writer: &mut Writer,
) -> io::Result<()>
where
    R: CommandRunner,
    Reader: BufRead,
    Writer: Write,
{
    run_blocking_loop(server, reader, writer)
}

#[cfg(unix)]
pub fn serve_stdio_signal_aware<Reader, Writer>(
    reader: Reader,
    writer: Writer,
) -> io::Result<Writer>
where
    Reader: Read + AsRawFd,
    Writer: Write,
{
    let mut server = McpServer::from_environment()?;
    serve_stdio_signal_aware_with_server(&mut server, reader, writer)
}

#[cfg(not(unix))]
pub fn serve_stdio_signal_aware<Reader, Writer>(
    mut reader: Reader,
    mut writer: Writer,
) -> io::Result<Writer>
where
    Reader: BufRead,
    Writer: Write,
{
    let mut server = McpServer::from_environment()?;
    run_blocking_loop(&mut server, &mut reader, &mut writer)?;
    Ok(writer)
}

#[cfg(unix)]
pub fn serve_stdio_signal_aware_with_server<R, Reader, Writer>(
    server: &mut McpServer<R>,
    mut reader: Reader,
    mut writer: Writer,
) -> io::Result<Writer>
where
    R: CommandRunner,
    Reader: Read + AsRawFd,
    Writer: Write,
{
    let signal_wakeup = SignalWakeup::new()?;
    run_signal_aware_loop(server, &mut reader, &mut writer, &signal_wakeup).map(|()| writer)
}

#[cfg(not(unix))]
pub fn serve_stdio_signal_aware_with_server<R, Reader, Writer>(
    server: &mut McpServer<R>,
    mut reader: Reader,
    mut writer: Writer,
) -> io::Result<Writer>
where
    R: CommandRunner,
    Reader: BufRead,
    Writer: Write,
{
    run_blocking_loop(server, &mut reader, &mut writer)?;
    Ok(writer)
}

fn run_blocking_loop<R, Reader, Writer>(
    server: &mut McpServer<R>,
    reader: &mut Reader,
    writer: &mut Writer,
) -> io::Result<()>
where
    R: CommandRunner,
    Reader: BufRead,
    Writer: Write,
{
    loop {
        let Some(message) = read_wire_message(reader)? else {
            break;
        };
        let request = match serde_json::from_str::<Value>(&message.body) {
            Ok(request) => request,
            Err(_) => {
                write_wire_message(
                    writer,
                    message.format,
                    &error_response(None, -32700, "parse error"),
                )?;
                continue;
            }
        };

        if let Some(response) = server.handle_json_rpc(request) {
            write_wire_message(writer, message.format, &response)?;
        }
        if server.shutdown_requested() {
            break;
        }
    }
    Ok(())
}

#[cfg(unix)]
struct SignalWakeup {
    read: UnixStream,
    write_fd: RawFd,
    previous_actions: Vec<(libc::c_int, libc::sigaction)>,
    _registration_lock: MutexGuard<'static, ()>,
}

#[cfg(unix)]
static SIGNAL_REGISTRATION_LOCK: Mutex<()> = Mutex::new(());
#[cfg(unix)]
static SIGNAL_WAKEUP_FD: AtomicI32 = AtomicI32::new(-1);

#[cfg(unix)]
impl SignalWakeup {
    fn new() -> io::Result<Self> {
        let (read, write) = UnixStream::pair()?;
        read.set_nonblocking(true)?;
        let registration_lock = SIGNAL_REGISTRATION_LOCK
            .lock()
            .map_err(|_| io::Error::other("signal registration lock is poisoned"))?;
        let write_fd = write.into_raw_fd();
        if let Err(err) = set_nonblocking(write_fd) {
            // SAFETY: write_fd was just transferred from write and is still owned here.
            unsafe {
                libc::close(write_fd);
            }
            return Err(err);
        }
        let mut wakeup = Self {
            read,
            write_fd,
            previous_actions: Vec::new(),
            _registration_lock: registration_lock,
        };
        SIGNAL_WAKEUP_FD.store(write_fd, Ordering::SeqCst);
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            wakeup.install(signal)?;
        }
        Ok(wakeup)
    }

    fn install(&mut self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: zeroed sigaction values are initialized below before use.
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = signal_wakeup_handler as *const () as usize;
        action.sa_flags = 0;
        // SAFETY: action contains a valid signal mask field.
        if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: sigaction receives valid pointers for a supported signal.
        let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
        if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.previous_actions.push((signal, previous));
        Ok(())
    }

    fn raw_fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    fn drain(&self) -> io::Result<()> {
        let mut read = &self.read;
        let mut buffer = [0_u8; 64];
        loop {
            match read.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err),
            }
        }
    }
}

#[cfg(unix)]
impl Drop for SignalWakeup {
    fn drop(&mut self) {
        SIGNAL_WAKEUP_FD.store(-1, Ordering::SeqCst);
        for (signal, previous) in self.previous_actions.drain(..).rev() {
            // SAFETY: previous was returned by sigaction for the same signal.
            unsafe {
                libc::sigaction(signal, &previous, std::ptr::null_mut());
            }
        }
        // SAFETY: write_fd is owned by this guard and closed exactly once here.
        unsafe {
            libc::close(self.write_fd);
        }
    }
}

#[cfg(unix)]
extern "C" fn signal_wakeup_handler(_: libc::c_int) {
    let write_fd = SIGNAL_WAKEUP_FD.load(Ordering::Relaxed);
    if write_fd < 0 {
        return;
    }
    let byte = [1_u8];
    // SAFETY: write is async-signal-safe and receives a valid byte buffer.
    unsafe {
        libc::write(write_fd, byte.as_ptr().cast(), byte.len());
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl reads flags from an owned valid descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl updates flags on an owned valid descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn run_signal_aware_loop<R, Reader, Writer>(
    server: &mut McpServer<R>,
    reader: &mut Reader,
    writer: &mut Writer,
    signal_wakeup: &SignalWakeup,
) -> io::Result<()>
where
    R: CommandRunner,
    Reader: Read + AsRawFd,
    Writer: Write,
{
    let mut decoder = WireDecoder::default();
    loop {
        match wait_for_stdio_event(reader.as_raw_fd(), signal_wakeup.raw_fd())? {
            StdioPollEvent::Signal => {
                signal_wakeup.drain()?;
                break;
            }
            StdioPollEvent::Input => {
                let mut buffer = [0_u8; 65_536];
                let read = reader.read(&mut buffer)?;
                if read == 0 {
                    while let Some(message) = decoder.next_message(true)? {
                        if handle_wire_message(server, writer, message)? {
                            return Ok(());
                        }
                    }
                    break;
                }
                decoder.push(&buffer[..read]);
                while let Some(message) = decoder.next_message(false)? {
                    if handle_wire_message(server, writer, message)? {
                        return Ok(());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
enum StdioPollEvent {
    Input,
    Signal,
}

#[cfg(unix)]
fn wait_for_stdio_event(input_fd: RawFd, signal_fd: RawFd) -> io::Result<StdioPollEvent> {
    let mut poll_fds = [
        libc::pollfd {
            fd: input_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
        libc::pollfd {
            fd: signal_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: poll receives a valid pointer to two initialized pollfd values.
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if result < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if poll_fds[1].revents != 0 {
            return Ok(StdioPollEvent::Signal);
        }
        if poll_fds[0].revents != 0 {
            return Ok(StdioPollEvent::Input);
        }
    }
}

fn handle_wire_message<R, Writer>(
    server: &mut McpServer<R>,
    writer: &mut Writer,
    message: WireMessage,
) -> io::Result<bool>
where
    R: CommandRunner,
    Writer: Write,
{
    let request = match serde_json::from_str::<Value>(&message.body) {
        Ok(request) => request,
        Err(_) => {
            write_wire_message(
                writer,
                message.format,
                &error_response(None, -32700, "parse error"),
            )?;
            return Ok(false);
        }
    };
    if let Some(response) = server.handle_json_rpc(request) {
        write_wire_message(writer, message.format, &response)?;
    }
    Ok(server.shutdown_requested())
}

#[derive(Default)]
struct WireDecoder {
    bytes: Vec<u8>,
}

impl WireDecoder {
    fn push(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn next_message(&mut self, eof: bool) -> io::Result<Option<WireMessage>> {
        loop {
            let Some((line_end, line_consumed)) = wire_line_bounds(&self.bytes, eof) else {
                return Ok(None);
            };
            let first_line = std::str::from_utf8(&self.bytes[..line_end])
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            if first_line.trim().is_empty() {
                self.bytes.drain(..line_consumed);
                continue;
            }
            let Some(length) = content_length(first_line) else {
                let body = first_line.to_string();
                self.bytes.drain(..line_consumed);
                return Ok(Some(WireMessage {
                    format: WireFormat::Line,
                    body,
                }));
            };

            let mut cursor = line_consumed;
            let header_end = loop {
                let Some((relative_end, relative_consumed)) =
                    wire_line_bounds(&self.bytes[cursor..], eof)
                else {
                    return Ok(None);
                };
                let header = std::str::from_utf8(&self.bytes[cursor..cursor + relative_end])
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                cursor += relative_consumed;
                if header.trim().is_empty() {
                    break cursor;
                }
            };
            let body_end = header_end.checked_add(length).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "content length overflow")
            })?;
            if self.bytes.len() < body_end {
                if eof {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "content length body is incomplete",
                    ));
                }
                return Ok(None);
            }
            let body = String::from_utf8(self.bytes[header_end..body_end].to_vec())
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
            self.bytes.drain(..body_end);
            return Ok(Some(WireMessage {
                format: WireFormat::ContentLength,
                body,
            }));
        }
    }
}

fn wire_line_bounds(bytes: &[u8], eof: bool) -> Option<(usize, usize)> {
    if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
        let line_end = if newline > 0 && bytes[newline - 1] == b'\r' {
            newline - 1
        } else {
            newline
        };
        return Some((line_end, newline + 1));
    }
    (eof && !bytes.is_empty()).then_some((bytes.len(), bytes.len()))
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum WireFormat {
    Line,
    ContentLength,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct WireMessage {
    format: WireFormat,
    body: String,
}

fn read_wire_message<R: BufRead>(reader: &mut R) -> io::Result<Option<WireMessage>> {
    loop {
        let mut first_line = String::new();
        if reader.read_line(&mut first_line)? == 0 {
            return Ok(None);
        }

        if first_line.trim().is_empty() {
            continue;
        }

        if let Some(length) = content_length(&first_line) {
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header)? == 0 {
                    return Ok(None);
                }
                if header.trim().is_empty() {
                    break;
                }
            }

            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            let body = String::from_utf8(body)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;

            return Ok(Some(WireMessage {
                format: WireFormat::ContentLength,
                body,
            }));
        }

        return Ok(Some(WireMessage {
            format: WireFormat::Line,
            body: first_line.trim_end_matches(['\r', '\n']).to_string(),
        }));
    }
}

fn write_wire_message<W: Write>(
    writer: &mut W,
    format: WireFormat,
    response: &Value,
) -> io::Result<()> {
    let body = response.to_string();
    match format {
        WireFormat::Line => {
            writeln!(writer, "{body}")?;
        }
        WireFormat::ContentLength => {
            write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len())?;
        }
    }
    writer.flush()
}

fn content_length(line: &str) -> Option<usize> {
    let (name, value) = line.split_once(':')?;
    if name.trim().eq_ignore_ascii_case("content-length") {
        value.trim().parse().ok()
    } else {
        None
    }
}
