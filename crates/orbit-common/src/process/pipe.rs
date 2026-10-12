//! Pipe descriptor helpers shared by the bounded runner, the output reader and
//! the stdin writer supervisors.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

/// Put `fd` into non-blocking mode and keep its other status flags. Callers
/// must poll before each read or write, so a stalled pipe cannot block the
/// supervising thread.
pub fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // Safety: `fd` is an open descriptor borrowed from a live owner. The two
    // `fcntl` calls only read and replace its status flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
