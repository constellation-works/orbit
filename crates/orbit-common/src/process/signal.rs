//! Identity-bound Unix signalling. Numeric PID/PGID signals cannot close a
//! check/use race, so unsupported kernels fail closed instead of using kill.

use std::io;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::identity::{
    ProbeOutcome, is_stable_token, legacy_lstart_matches, probe_process_start_identity,
    stable_tokens_match,
};

/// A process identity retained across signals and waits. Linux uses a pidfd;
/// macOS uses libproc's unique process id and audited, pid-version-bound signals.
#[cfg(unix)]
pub struct BoundProcess {
    pid: u32,
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(target_os = "macos")]
    unique_id: u64,
}

#[cfg(unix)]
impl BoundProcess {
    /// Capture the current process, optionally requiring a persisted start token.
    /// An absent or mismatched original returns None; unreadable identity and
    /// unavailable binding primitives are errors, never numeric-signal fallbacks.
    pub fn bind(pid: u32, expected: Option<&str>) -> io::Result<Option<Self>> {
        if pid <= 1 || pid > i32::MAX as u32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid signal pid",
            ));
        }
        #[cfg(target_os = "linux")]
        let process = {
            use std::os::fd::FromRawFd;
            // SAFETY: pidfd_open returns a new descriptor owned by this call.
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
            if raw < 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            Self {
                pid,
                fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) },
            }
        };
        #[cfg(target_os = "macos")]
        let process = match darwin_info(pid)? {
            Some(info) => Self {
                pid,
                unique_id: info.unique.unique_id,
            },
            None => return Ok(None),
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = expected;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "identity-bound signalling unavailable",
            ))
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            if let Some(expected) = expected {
                let matches = if is_stable_token(expected) {
                    match probe_process_start_identity(pid) {
                        ProbeOutcome::Token(current) => stable_tokens_match(expected, &current),
                        ProbeOutcome::NoProcess => {
                            return if process.is_alive()? {
                                Err(io::Error::other(
                                    "process identity absent while bound process is live",
                                ))
                            } else {
                                Ok(None)
                            };
                        }
                        ProbeOutcome::Unavailable => {
                            return Err(io::Error::other("process identity unavailable"));
                        }
                    }
                } else {
                    if !legacy_lstart_matches(pid, expected) {
                        return if process.is_alive()? {
                            Err(io::Error::other(
                                "legacy process identity cannot be verified",
                            ))
                        } else {
                            Ok(None)
                        };
                    }
                    true
                };
                if !matches {
                    return Ok(None);
                }
            }
            // The token probe above could have observed a replacement. Require
            // the captured identity to still be alive after reading the token.
            Ok(process.is_alive()?.then_some(process))
        }
    }

    /// Whether this original process is still executing, ignoring PID reuse.
    pub fn is_alive(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut fd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd, retained descriptor, nonblocking query.
            let rc = unsafe { libc::poll(&mut fd, 1, 0) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            if fd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::other("invalid process descriptor"));
            }
            // The pidfd becomes readable at exit, before zombie reaping.
            Ok(rc == 0)
        }
        #[cfg(target_os = "macos")]
        {
            Ok(darwin_info(self.pid)?.is_some_and(|info| {
                info.unique.unique_id == self.unique_id && info.bsd.pbi_status != libc::SZOMB
            }))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "identity-bound liveness unavailable",
        ))
    }

    /// Deliver a signal to this identity, or return false if it has exited.
    /// Even exit/reuse between a probe and the syscall cannot signal a replacement.
    pub fn signal(&self, signal: libc::c_int) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        let error = {
            use std::os::fd::AsRawFd;
            // SAFETY: valid owned pidfd, signal, no siginfo, no flags.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if rc == 0 {
                return Ok(true);
            }
            io::Error::last_os_error()
        };
        #[cfg(target_os = "macos")]
        let error = {
            let Some(info) = darwin_info(self.pid)? else {
                return Ok(false);
            };
            if info.unique.unique_id != self.unique_id {
                return Ok(false);
            }
            // Exec can change pidversion without changing unique_id. Refresh it
            // only for the same unique_id; an exec during this syscall fails
            // closed and is not mistaken for the original process's exit.
            let mut token = [0u32; 8];
            token[5] = self.pid;
            token[7] = info.unique.id_version as u32;
            unsafe extern "C" {
                fn proc_signal_with_audittoken(
                    token: *mut [u32; 8],
                    signal: libc::c_int,
                ) -> libc::c_int;
            }
            // SAFETY: audit_token_t is eight u32 values; libproc reads it.
            let rc = unsafe { proc_signal_with_audittoken(&mut token, signal) };
            if rc == 0 {
                return Ok(true);
            }
            if rc == libc::ESRCH && self.is_alive()? {
                return Err(io::Error::other("process exec raced identity-bound signal"));
            }
            io::Error::from_raw_os_error(rc)
        };
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let error = {
            let _ = signal;
            io::Error::new(
                io::ErrorKind::Unsupported,
                "identity-bound signalling unavailable",
            )
        };
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    /// Current group membership, accepted only while the captured process lives.
    pub fn process_group(&self) -> io::Result<Option<libc::pid_t>> {
        // SAFETY: getpgid is an observational query; it delivers no signal.
        let group = unsafe { libc::getpgid(self.pid as libc::pid_t) };
        let error = (group < 0).then(io::Error::last_os_error);
        if !self.is_alive()? {
            return Ok(None);
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(Some(group))
    }
}

/// Snapshot numeric group members for subsequent identity binding. This list
/// never authorizes signalling: bind every member and recheck the leader before
/// use. Callers must report incomplete teardown if unbound members remain.
#[cfg(unix)]
pub fn process_group_members(pgid: libc::pid_t) -> io::Result<Vec<u32>> {
    #[cfg(target_os = "linux")]
    {
        let mut members = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            // SAFETY: observational query only.
            if unsafe { libc::getpgid(pid as libc::pid_t) } == pgid {
                members.push(pid);
            }
        }
        Ok(members)
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: null/zero requests the number of process slots.
        let size = unsafe { libc::proc_listpgrppids(pgid, std::ptr::null_mut(), 0) };
        if size <= 0 {
            return Err(io::Error::last_os_error());
        }
        let mut pids = vec![0i32; size as usize + 64];
        let bytes =
            i32::try_from(pids.len() * std::mem::size_of::<i32>()).map_err(io::Error::other)?;
        // SAFETY: the vector provides exactly `bytes` writable bytes.
        let written = unsafe { libc::proc_listpgrppids(pgid, pids.as_mut_ptr().cast(), bytes) };
        if written <= 0 {
            return Err(io::Error::last_os_error());
        }
        if written as usize >= pids.len() {
            return Err(io::Error::other("process group snapshot overflow"));
        }
        pids.truncate(written as usize);
        Ok(pids
            .into_iter()
            .filter_map(|pid| u32::try_from(pid).ok().filter(|pid| *pid > 1))
            .collect())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pgid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "process group snapshot unavailable",
        ))
    }
}

// XNU's libproc ABI (PROC_PIDT_BSDINFOWITHUNIQID). The BSD and unique identity
// are returned in one kernel snapshot. See apple-oss-distributions/xnu,
// bsd/sys/proc_info_private.h and bsd/kern/proc_info.c.
#[cfg(target_os = "macos")]
#[repr(C)]
struct DarwinUniqueInfo {
    uuid: [u8; 16],
    unique_id: u64,
    parent_unique_id: u64,
    id_version: i32,
    original_parent_version: i32,
    reserved: [u64; 2],
}
#[cfg(target_os = "macos")]
#[repr(C)]
struct DarwinInfo {
    bsd: libc::proc_bsdinfo,
    unique: DarwinUniqueInfo,
}

#[cfg(target_os = "macos")]
fn darwin_info(pid: u32) -> io::Result<Option<DarwinInfo>> {
    let mut info = std::mem::MaybeUninit::<DarwinInfo>::uninit();
    let size = std::mem::size_of::<DarwinInfo>() as i32;
    // SAFETY: writable buffer with the exact size of libproc's flavor 18 ABI.
    let written = unsafe { libc::proc_pidinfo(pid as i32, 18, 0, info.as_mut_ptr().cast(), size) };
    if written == size {
        return Ok(Some(unsafe { info.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if written == 0 && error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[path = "tests/signal.rs"]
mod tests;
