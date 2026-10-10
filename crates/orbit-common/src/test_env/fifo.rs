//! One-shot fixture handshakes, with bounded writer-side admission.

use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// Create a FIFO before starting a fixture that blocks on a shell `read`.
pub fn create_fixture_fifo(path: &Path) -> io::Result<()> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `path` is a live NUL-terminated path; mkfifo retains no pointers.
    if unsafe { libc::mkfifo(path.as_ptr(), 0o600) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Release a FIFO reader without blocking forever if the child failed to open it.
///
/// Retry the nonblocking writer open in this process until `deadline`. The
/// one-line message fits in a pipe's atomic-write capacity.
pub fn release_fixture_fifo(path: &Path, deadline: Instant) -> io::Result<()> {
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
        {
            Ok(mut writer) => return writer.write_all(b"go\n"),
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "fixture FIFO has no reader",
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}
