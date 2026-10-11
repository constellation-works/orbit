//! Cleanup for long-lived CLI fixture processes, including assertion unwinds.

use std::io;
use std::ops::{Deref, DerefMut};
#[cfg(unix)]
use std::process::Stdio;
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::{Duration, Instant};

/// Own a child immediately after spawning, before any readiness assertions.
///
/// The original PID remains valid across an executable handover. Cleanup
/// checks for an already-reaped child before signaling, so an explicit wait
/// never causes a later drop to signal a reused PID.
pub(crate) struct ChildGuard {
    child: Child,
    #[cfg(unix)]
    _group_anchor: Option<GroupAnchor>,
}

impl ChildGuard {
    pub(crate) fn new(child: Child) -> Self {
        Self {
            child,
            #[cfg(unix)]
            _group_anchor: None,
        }
    }

    /// Spawn a server in an owned process group whose identity stays reserved
    /// even after callers explicitly reap the server.
    // Other integration binaries use only the single-child constructor.
    #[allow(dead_code)]
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // An unreaped direct child reserves its PID and thus this group ID.
            // Keep stdin open; read is a shell builtin, not a polling subprocess.
            let anchor = GroupAnchor(
                Command::new("/bin/sh")
                    .args(["-c", "read -r release"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn()?,
            );
            command.process_group(anchor.0.id() as libc::pid_t);
            let mut guard = Self::new(command.spawn()?);
            guard._group_anchor = Some(anchor);
            Ok(guard)
        }
        #[cfg(not(unix))]
        {
            command.spawn().map(Self::new)
        }
    }
}

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let reaped = matches!(self.child.try_wait(), Ok(Some(_)));
        if reaped {
            return;
        }
        #[cfg(unix)]
        {
            // SAFETY: this PID belongs to our unreaped child, even after exec.
            // Cleanup must not panic while unwinding an assertion failure.
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => break,
                }
            }
        }
        // SIGKILL on Unix; also supplies cleanup on platforms without SIGTERM.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// The anchor is never exposed to callers or reaped before group cleanup.
#[cfg(unix)]
struct GroupAnchor(Child);

#[cfg(unix)]
impl Drop for GroupAnchor {
    fn drop(&mut self) {
        // SAFETY: this direct child has never been waited/reaped. Its PID
        // cannot be reused, even if it exited, and reserves our group ID.
        unsafe { libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL) };
        let _ = self.0.wait();
    }
}
