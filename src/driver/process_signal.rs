use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);

pub(super) struct ProcessSignalGuard {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl ProcessSignalGuard {
    pub(super) fn install() -> io::Result<Self> {
        SIGNAL_RECEIVED.store(false, Ordering::SeqCst);
        let mut guard = Self {
            previous: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            guard.install_signal(signal)?;
        }
        Ok(guard)
    }

    pub(super) fn received(&self) -> bool {
        SIGNAL_RECEIVED.load(Ordering::SeqCst)
    }

    fn install_signal(&mut self, signal: libc::c_int) -> io::Result<()> {
        // SAFETY: zeroed sigaction values are initialized before registration.
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = signal_handler as *const () as usize;
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
        self.previous.push((signal, previous));
        Ok(())
    }
}

impl Drop for ProcessSignalGuard {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.drain(..).rev() {
            // SAFETY: previous was returned by sigaction for the same signal.
            unsafe {
                libc::sigaction(signal, &previous, std::ptr::null_mut());
            }
        }
    }
}

extern "C" fn signal_handler(_: libc::c_int) {
    SIGNAL_RECEIVED.store(true, Ordering::Relaxed);
}
