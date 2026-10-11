//! Identity-bound cleanup for a backend owned and reaped by another process.
//!
//! Readiness carries a kernel-bound Linux peer pidfd or the backend's own
//! macOS identity. Cleanup signals the retained identity directly, without a
//! racy liveness-probe/kill sequence or adoption of a stale PID marker.

use std::io;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct BackendIdentity {
    pub(super) pid: libc::pid_t,
    generation: u64,
    exec_version: u32,
}

pub(super) struct BackendReadiness(platform::Readiness);

impl BackendReadiness {
    pub(super) fn new(workspace: &std::path::Path) -> io::Result<Self> {
        platform::Readiness::new(workspace).map(Self)
    }

    pub(super) fn receive(&self) -> io::Result<Option<BackendGuard>> {
        self.0.receive()
    }
}

pub(super) struct ReadyLease {
    _inner: platform::ReadyLease,
}

pub(super) fn publish_readiness(workspace: &std::path::Path) -> io::Result<ReadyLease> {
    platform::publish_readiness(workspace).map(|inner| ReadyLease { _inner: inner })
}

pub(super) struct BackendGuard {
    pub(super) identity: BackendIdentity,
    handle: Option<platform::ProcessHandle>,
}

impl BackendGuard {
    pub(super) fn acquire(identity: BackendIdentity) -> io::Result<Self> {
        if identity.pid <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "non-positive backend PID",
            ));
        }
        Ok(Self {
            identity,
            handle: platform::ProcessHandle::open(identity)?,
        })
    }

    pub(super) fn is_live(&self) -> io::Result<bool> {
        self.handle
            .as_ref()
            .map_or(Ok(false), |handle| handle.is_live())
    }
}

impl Drop for BackendGuard {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            // Errors (including an already exited backend) cannot panic during
            // unwinding. Neither platform falls back to signaling a bare PID.
            let _ = handle.kill();
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::hash::{Hash, Hasher};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::path::Path;

    use super::*;

    pub(super) fn identity(pid: libc::pid_t) -> io::Result<Option<BackendIdentity>> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // comm can contain spaces and parentheses. Field 22 (starttime) is
        // index 19 after the final ')', where field 3 (state) begins.
        let generation = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(19))
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid process stat"))?;
        Ok(Some(BackendIdentity {
            pid,
            generation,
            exec_version: 0,
        }))
    }

    pub(super) struct ProcessHandle(OwnedFd);

    pub(super) struct Readiness(UnixListener);
    pub(super) type ReadyLease = UnixStream;

    fn readiness_address(workspace: &Path) -> io::Result<SocketAddr> {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        // Orbit supplies the backend a canonical workspace root. Match it
        // even when the test's temporary root has a symlinked ancestor.
        workspace.canonicalize()?.hash(&mut hash);
        // The disposable workspace's random parent directory distinguishes
        // fixtures. An abstract address avoids Unix sun_path limits when the
        // executor's TMPDIR is deep, and vanishes when the listener closes.
        SocketAddr::from_abstract_name(format!("orbit-signal-fixture-{:x}", hash.finish()))
    }

    pub(super) fn publish_readiness(workspace: &Path) -> io::Result<ReadyLease> {
        // Landlock does not grant the backend /proc reads. A connection lets
        // the kernel supply its process identity without widening fs grants.
        UnixStream::connect_addr(&readiness_address(workspace)?)
    }

    impl Readiness {
        pub(super) fn new(workspace: &Path) -> io::Result<Self> {
            let listener = UnixListener::bind_addr(&readiness_address(workspace)?)?;
            listener.set_nonblocking(true)?;
            Ok(Self(listener))
        }

        pub(super) fn receive(&self) -> io::Result<Option<BackendGuard>> {
            let stream = match self.0.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(error),
            };
            let mut credentials = libc::ucred {
                pid: 0,
                uid: 0,
                gid: 0,
            };
            socket_option(&stream, libc::SO_PEERCRED, &mut credentials)?;
            let mut fd: libc::c_int = -1;
            // Linux 6.5+ SO_PEERPIDFD, absent from the libc bindings. It uses
            // the socket's retained struct pid, not a lookup of a reusable PID.
            // https://github.com/torvalds/linux/blob/v6.5/net/core/sock.c
            socket_option(&stream, 77, &mut fd)?;
            // Safety: successful SO_PEERPIDFD returned a fresh owned pidfd.
            let handle = ProcessHandle(unsafe { OwnedFd::from_raw_fd(fd) });
            Ok(Some(BackendGuard {
                identity: BackendIdentity {
                    pid: credentials.pid,
                    generation: 0,
                    exec_version: 0,
                },
                handle: Some(handle),
            }))
        }
    }

    fn socket_option<T>(stream: &UnixStream, option: libc::c_int, value: &mut T) -> io::Result<()> {
        let mut size = std::mem::size_of::<T>() as libc::socklen_t;
        // Safety: value points to an initialized buffer of the supplied size;
        // callers use the kernel ABI type for SO_PEERCRED or SO_PEERPIDFD.
        let rc = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                std::ptr::from_mut(value).cast(),
                &mut size,
            )
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    impl ProcessHandle {
        pub(super) fn open(expected: BackendIdentity) -> io::Result<Option<Self>> {
            if identity(expected.pid)? != Some(expected) {
                return Ok(None);
            }
            // Safety: pidfd_open takes a positive PID and flags; it returns a
            // new owned descriptor. No signal is delivered while acquiring it.
            let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, expected.pid, 0) };
            if fd < 0 {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            // Safety: successful pidfd_open returned a fresh owned FD.
            let handle = Self(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) });
            // Recheck after opening: exit/reap/reuse between the first probe
            // and pidfd_open must not bind us to the replacement process.
            Ok((identity(expected.pid)? == Some(expected)).then_some(handle))
        }

        pub(super) fn is_live(&self) -> io::Result<bool> {
            let mut pollfd = libc::pollfd {
                fd: self.0.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // Safety: pollfd is initialized, valid for one entry, and the
            // owned pidfd remains open. A zero timeout never blocks.
            let rc = unsafe { libc::poll(&mut pollfd, 1, 0) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            if pollfd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid pidfd"));
            }
            // pidfd becomes readable at exit, including before zombie reaping.
            Ok(rc == 0)
        }

        pub(super) fn kill(&self) -> io::Result<()> {
            // Safety: signaling is bound to this owned pidfd, even after its
            // original process exits and the numeric PID is reused.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if rc < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::{Path, PathBuf};

    use super::*;

    pub(super) struct Readiness(PathBuf);
    pub(super) struct ReadyLease;

    impl Readiness {
        pub(super) fn new(workspace: &Path) -> io::Result<Self> {
            Ok(Self(workspace.join("markers/listen.ready")))
        }

        pub(super) fn receive(&self) -> io::Result<Option<BackendGuard>> {
            let contents = match std::fs::read(&self.0) {
                Ok(contents) => contents,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let Ok(identity) = serde_json::from_slice(&contents) else {
                return Ok(None);
            };
            BackendGuard::acquire(identity).map(Some)
        }
    }

    pub(super) fn publish_readiness(workspace: &Path) -> io::Result<ReadyLease> {
        let identity = identity(std::process::id() as libc::pid_t)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "backend process identity unavailable",
            )
        })?;
        let marker = serde_json::to_vec(&identity)?;
        std::fs::write(workspace.join("markers/listen.ready"), marker)?;
        Ok(ReadyLease)
    }

    // XNU's proc_info_private.h: PROC_PIDUNIQIDENTIFIERINFO and its stable
    // 56-byte API layout. libc exposes proc_pidinfo but not this flavor.
    // https://github.com/apple-oss-distributions/xnu/blob/xnu-10063.121.3/bsd/sys/proc_info_private.h
    #[repr(C)]
    #[derive(Default)]
    struct ProcUniqueInfo {
        uuid: [u8; 16],
        unique_id: u64,
        parent_unique_id: u64,
        id_version: i32,
        original_parent_id_version: i32,
        reserved: [u64; 2],
    }

    unsafe extern "C" {
        // libproc.h: audit_token_t is eight u32 values. The kernel checks
        // val[5] (PID) and val[7] (pidversion) while holding the process ref.
        // https://github.com/apple-oss-distributions/xnu/blob/xnu-10063.121.3/bsd/kern/proc_info.c
        fn proc_signal_with_audittoken(token: *const [u32; 8], signal: libc::c_int) -> libc::c_int;
    }

    pub(super) fn identity(pid: libc::pid_t) -> io::Result<Option<BackendIdentity>> {
        let mut info = ProcUniqueInfo::default();
        let size = std::mem::size_of::<ProcUniqueInfo>() as libc::c_int;
        // Safety: info has the ABI layout and exact buffer size for flavor 17.
        // arg=0 excludes zombies; only the original live process can match.
        let rc =
            unsafe { libc::proc_pidinfo(pid, 17, 0, std::ptr::from_mut(&mut info).cast(), size) };
        if rc == size {
            return Ok(Some(BackendIdentity {
                pid,
                generation: info.unique_id,
                exec_version: info.id_version as u32,
            }));
        }
        let error = io::Error::last_os_error();
        if rc == 0 && error.raw_os_error() == Some(libc::ESRCH) {
            Ok(None)
        } else if rc == 0 {
            Err(error)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short process identity",
            ))
        }
    }

    pub(super) struct ProcessHandle(BackendIdentity);

    impl ProcessHandle {
        pub(super) fn open(expected: BackendIdentity) -> io::Result<Option<Self>> {
            Ok((identity(expected.pid)? == Some(expected)).then_some(Self(expected)))
        }

        pub(super) fn is_live(&self) -> io::Result<bool> {
            Ok(identity(self.0.pid)? == Some(self.0))
        }

        pub(super) fn kill(&self) -> io::Result<()> {
            let mut token = [0; 8];
            token[5] = self.0.pid as u32;
            token[7] = self.0.exec_version;
            // Safety: the token has audit_token_t's layout. The kernel compares
            // pidversion and signals that referenced process atomically, so a
            // reused PID or exec between probe and signal is refused with ESRCH.
            let rc = unsafe { proc_signal_with_audittoken(&token, libc::SIGKILL) };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(rc))
            }
        }
    }

    #[test]
    fn audit_token_mismatch_refuses_signal_to_owned_child() {
        let mut child = super::spawn_sleeper();
        let mut identity = identity(child.id() as libc::pid_t).unwrap().unwrap();
        identity.exec_version ^= 1;
        let handle = ProcessHandle(identity);
        assert!(!handle.is_live().unwrap());
        assert_eq!(handle.kill().unwrap_err().raw_os_error(), Some(libc::ESRCH));
        assert!(
            child.try_wait().unwrap().is_none(),
            "mismatched audit token killed child"
        );
    }
}

fn spawn_sleeper() -> super::ChildGuard {
    super::ChildGuard(
        std::process::Command::new("/bin/sleep")
            .arg("120")
            .spawn()
            .unwrap(),
    )
}

#[test]
fn backend_identity_mismatch_does_not_adopt_or_kill_owned_child() {
    let mut child = spawn_sleeper();
    let mut identity = platform::identity(child.id() as libc::pid_t)
        .unwrap()
        .unwrap();
    identity.generation ^= 1;
    let backend = BackendGuard::acquire(identity).unwrap();
    assert!(
        !backend.is_live().unwrap(),
        "replacement identity reported as live backend"
    );
    drop(backend);
    assert!(
        child.try_wait().unwrap().is_none(),
        "stale marker killed replacement child"
    );
}

#[test]
fn backend_guard_cleans_up_owned_child_on_panic() {
    let mut child = spawn_sleeper();
    let identity = platform::identity(child.id() as libc::pid_t)
        .unwrap()
        .unwrap();
    let panic = std::panic::catch_unwind(|| {
        let backend = BackendGuard::acquire(identity).unwrap();
        assert!(backend.is_live().unwrap());
        panic!("exercise backend cleanup during assertion unwind");
    });
    assert!(panic.is_err());
    let status = super::wait_with_deadline(&mut child, super::SHUTDOWN_DEADLINE)
        .expect("panic cleanup must terminate the owned backend");
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

#[test]
fn backend_handle_tracks_exit_and_reaping_without_signaling_another_pid() {
    let mut child = spawn_sleeper();
    let identity = platform::identity(child.id() as libc::pid_t)
        .unwrap()
        .unwrap();
    let mut backend = BackendGuard::acquire(identity).unwrap();
    assert!(backend.is_live().unwrap());
    child.kill().unwrap();
    let deadline = std::time::Instant::now() + super::SHUTDOWN_DEADLINE;
    while backend.is_live().unwrap() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        !backend.is_live().unwrap(),
        "unreaped backend must count as exited"
    );
    child.wait().unwrap();
    assert!(!backend.is_live().unwrap());

    // Model numeric PID reuse without churn: redirect the diagnostic PID to a
    // second owned child. Liveness and Drop must still use the old handle.
    let mut replacement = spawn_sleeper();
    backend.identity.pid = replacement.id() as libc::pid_t;
    assert!(
        !backend.is_live().unwrap(),
        "replacement PID revived exited backend"
    );
    drop(backend);
    assert!(
        replacement.try_wait().unwrap().is_none(),
        "cleanup signaled replacement PID"
    );
}
