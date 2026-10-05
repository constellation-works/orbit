//! Kernel peer authentication (design §4.2).
//!
//! The broker authenticates each connection, never a secret the client
//! presents: an agent can read its own environment and, on macOS, other
//! same-user processes' environments too. The kernel reports the connecting
//! process, and the broker accepts it only when it runs as this host's UID
//! inside the sandbox spawned for this run.
//!
//! - **Linux.** Bubblewrap gives every run its own PID namespace. The peer's
//!   `/proc/<pid>/ns/pid` must be the namespace whose leader the host found
//!   beneath the spawned `bwrap`, and that leader must still be the same
//!   process ([`namespace_key`]), so a recycled namespace inode cannot pass.
//!   `SO_PEERPIDFD` pins the peer where the kernel has it, so a recycled PID
//!   cannot be substituted between the credential read and the namespace read.
//! - **macOS.** There are no PID namespaces. The peer's parent chain, with each
//!   parent's start time checked against its child's, must reach the
//!   `sandbox-exec` process spawned for this run. An orphan reparented to
//!   `launchd` fails closed.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use orbit_common::OrbitError;
use orbit_common::process::ancestry::{ProcessStartKey, process_start_key};

#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use crate::runtime::recovery_authority::{PROC_ROOT, namespace_key, worker_namespace_leader};

/// Parent links followed before an ancestry walk gives up.
#[cfg(any(target_os = "macos", test))]
const MAX_ANCESTRY_DEPTH: usize = 256;

/// What a peer must belong to: this run's sandbox.
#[derive(Debug)]
pub(crate) enum PeerAnchor {
    /// The Bubblewrap PID namespace of this run.
    #[cfg(target_os = "linux")]
    Namespace(NamespaceAnchor),
    /// The `sandbox-exec` process of this run; peers must descend from it.
    /// Production uses it on macOS; tests exercise the same walk on Linux.
    #[cfg(any(target_os = "macos", test))]
    Ancestor(ProcessStartKey),
}

impl PeerAnchor {
    /// The anchor for the sandbox process spawned for this run.
    pub(crate) fn for_sandbox(sandbox_pid: u32) -> Result<Self, OrbitError> {
        #[cfg(target_os = "linux")]
        {
            let leader = worker_namespace_leader(sandbox_pid)?;
            Ok(Self::Namespace(NamespaceAnchor::for_leader(leader)?))
        }
        #[cfg(target_os = "macos")]
        {
            process_start_key(sandbox_pid)
                .map(Self::Ancestor)
                .ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "sandbox process {sandbox_pid} exited before the plugin broker identified it"
                    ))
                })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = sandbox_pid;
            Err(OrbitError::Execution(
                "the plugin broker authenticates peers only on Linux and macOS".to_string(),
            ))
        }
    }
}

/// A PID namespace, named by its leader as the host sees it.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct NamespaceAnchor {
    leader_pid: u32,
    leader_key: String,
    namespace: PathBuf,
}

#[cfg(target_os = "linux")]
impl NamespaceAnchor {
    /// Anchor to the namespace `leader_pid` belongs to.
    fn for_leader(leader_pid: u32) -> Result<Self, OrbitError> {
        let proc_root = Path::new(PROC_ROOT);
        let namespace = std::fs::read_link(proc_root.join(leader_pid.to_string()).join("ns/pid"))?;
        let leader_key = namespace_key(proc_root, leader_pid)?;
        Ok(Self {
            leader_pid,
            leader_key,
            namespace,
        })
    }

    fn admits(&self, pid: u32) -> Result<(), String> {
        let proc_root = Path::new(PROC_ROOT);
        let namespace = std::fs::read_link(proc_root.join(pid.to_string()).join("ns/pid"))
            .map_err(|error| format!("peer PID namespace unreadable: {error}"))?;
        if namespace != self.namespace {
            return Err(format!(
                "peer PID namespace {} is not this run's sandbox",
                namespace.display()
            ));
        }
        // The namespace lives exactly as long as its leader. A leader with
        // the same start identity proves the inode still names this run's
        // namespace rather than a later one that reused it.
        if namespace_key(proc_root, self.leader_pid).ok().as_deref() != Some(&self.leader_key) {
            return Err("this run's sandbox namespace has ended".to_string());
        }
        Ok(())
    }
}

/// A connected peer as the kernel identified it at accept.
#[derive(Debug)]
pub(crate) struct Peer {
    pub(crate) pid: u32,
    start: ProcessStartKey,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
}

/// Why a connection was refused. Logged by the host; never sent to the peer.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub(crate) pid: Option<u32>,
    pub(crate) reason: String,
}

impl Refusal {
    fn new(pid: Option<u32>, reason: impl Into<String>) -> Self {
        Self {
            pid,
            reason: reason.into(),
        }
    }
}

/// Identify the peer of `stream` and check it belongs to `anchor`.
pub(crate) fn authenticate(stream: &UnixStream, anchor: &PeerAnchor) -> Result<Peer, Refusal> {
    let fd = stream.as_raw_fd();
    let (pid, uid) = peer_credentials(fd)
        .map_err(|error| Refusal::new(None, format!("peer credentials unavailable: {error}")))?;
    // SAFETY: `geteuid` only reads the calling process's credentials.
    let euid = unsafe { libc::geteuid() };
    if uid != euid {
        return Err(Refusal::new(
            Some(pid),
            format!("peer uid {uid} is not this host's uid {euid}"),
        ));
    }
    if pid == 0 {
        return Err(Refusal::new(
            None,
            "peer is not visible from this host's PID namespace",
        ));
    }
    #[cfg(target_os = "linux")]
    let pidfd = peer_pidfd(fd);
    let start = process_start_key(pid).ok_or_else(|| Refusal::new(Some(pid), "peer exited"))?;
    let peer = Peer {
        pid,
        start,
        #[cfg(target_os = "linux")]
        pidfd,
    };
    reauthenticate(&peer, anchor)?;
    Ok(peer)
}

/// Check an authenticated peer again, after its request was read and before
/// anything acts on it. A peer that exited in between is refused.
pub(crate) fn reauthenticate(peer: &Peer, anchor: &PeerAnchor) -> Result<(), Refusal> {
    let membership = match anchor {
        #[cfg(target_os = "linux")]
        PeerAnchor::Namespace(namespace) => namespace.admits(peer.pid),
        #[cfg(any(target_os = "macos", test))]
        PeerAnchor::Ancestor(sandbox) => descends_from(peer.pid, *sandbox),
    };
    membership.map_err(|reason| Refusal::new(Some(peer.pid), reason))?;
    // The membership reads above went through a PID. Confirm it still names
    // the process that connected: a live pidfd, or an unchanged start time.
    if !peer_unchanged(peer) {
        return Err(Refusal::new(Some(peer.pid), "peer exited"));
    }
    Ok(())
}

fn peer_unchanged(peer: &Peer) -> bool {
    #[cfg(target_os = "linux")]
    {
        if peer.pidfd.as_ref().is_some_and(|pidfd| !pidfd_alive(pidfd)) {
            return false;
        }
        if orbit_common::process::identity::linux_process_state(peer.pid)
            .is_none_or(|(state, _)| state == 'Z')
        {
            return false;
        }
    }
    process_start_key(peer.pid) == Some(peer.start)
}

/// Walk `pid`'s parent chain to `sandbox`. Each parent must have started no
/// later than its child, so a recycled parent PID ends the walk.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn descends_from(pid: u32, sandbox: ProcessStartKey) -> Result<(), String> {
    use orbit_common::process::ancestry::process_start_and_parent;

    let (mut current, mut parent) =
        process_start_and_parent(pid).ok_or_else(|| "peer exited".to_string())?;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if current == sandbox {
            return Ok(());
        }
        if current.pid == sandbox.pid {
            return Err("this run's sandbox process has ended".to_string());
        }
        if parent <= 1 || parent == current.pid {
            return Err("peer does not descend from this run's sandbox".to_string());
        }
        let (next, next_parent) = process_start_and_parent(parent)
            .ok_or_else(|| "peer ancestry ended at an exited process".to_string())?;
        if next.starttime > current.starttime {
            return Err("peer ancestry passes through a reused PID".to_string());
        }
        current = next;
        parent = next_parent;
    }
    Err("peer ancestry is deeper than the broker follows".to_string())
}

/// The peer's host PID and effective UID.
#[cfg(target_os = "linux")]
fn peer_credentials(fd: std::os::fd::RawFd) -> io::Result<(u32, u32)> {
    // SAFETY: `ucred` is plain data; all-zero is a valid value.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe `credentials` exactly.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u32::try_from(credentials.pid).unwrap_or(0), credentials.uid))
}

/// A pidfd for the peer, on kernels with `SO_PEERPIDFD` (Linux 6.5+).
#[cfg(target_os = "linux")]
fn peer_pidfd(fd: std::os::fd::RawFd) -> Option<OwnedFd> {
    let mut pidfd: libc::c_int = -1;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe `pidfd` exactly.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERPIDFD,
            (&raw mut pidfd).cast(),
            &mut length,
        )
    };
    // SAFETY: on success the kernel returned a new descriptor we now own.
    (rc == 0 && pidfd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(pidfd) })
}

#[cfg(target_os = "linux")]
fn pidfd_alive(pidfd: &OwnedFd) -> bool {
    // SAFETY: signal 0 only probes; the pidfd is owned and open.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            0,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    rc == 0
}

/// The peer's PID (from its audit token, else `LOCAL_PEERPID`) and UID.
#[cfg(target_os = "macos")]
fn peer_credentials(fd: std::os::fd::RawFd) -> io::Result<(u32, u32)> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both out-pointers are valid for writes.
    if unsafe { libc::getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // `audit_token_t` is eight 32-bit words; the PID is word 5.
    let mut token = [0u32; 8];
    let mut length = std::mem::size_of_val(&token) as libc::socklen_t;
    // SAFETY: the buffer and its length describe `token` exactly.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if rc == 0 && length as usize == std::mem::size_of_val(&token) {
        return Ok((token[5], uid));
    }
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: the buffer and its length describe `pid` exactly.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u32::try_from(pid).unwrap_or(0), uid))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_credentials(_fd: std::os::fd::RawFd) -> io::Result<(u32, u32)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "peer credentials are read only on Linux and macOS",
    ))
}
