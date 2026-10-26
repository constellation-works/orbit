//! Binding the intake socket: resolving `lease.intake.peer_user`, and the
//! socket's directory, ownership, mode and stale-file handling.

use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use orbit_core::OrbitError;

use super::LEASE_INTAKE_SOCKET_PATH;

/// Group and owner only: the operator owns it, the peer user's group reaches it.
const RUN_DIR_MODE: u32 = 0o750;
const SOCKET_MODE: u32 = 0o660;

/// The OS user `lease.intake.peer_user` names, resolved at startup.
#[derive(Debug, Clone)]
pub(super) struct PeerUser {
    pub(super) name: String,
    pub(super) uid: libc::uid_t,
    pub(super) gid: libc::gid_t,
}

impl PeerUser {
    /// Resolve `name`, refusing root, the operator's own user and a user
    /// that does not exist.
    fn resolve(name: &str) -> Result<Self, OrbitError> {
        let Some((uid, gid)) = lookup_user(name)? else {
            return Err(OrbitError::InvalidInput(format!(
                "lease.intake.peer_user names `{name}`, which is not a user on this host; \
                 create the lease listener user first or unset the setting"
            )));
        };
        if uid == 0 {
            return Err(OrbitError::InvalidInput(format!(
                "lease.intake.peer_user must not be root (`{name}` has uid 0); name the lease \
                 listener's own user"
            )));
        }
        if uid == operator_uid() {
            return Err(OrbitError::InvalidInput(format!(
                "lease.intake.peer_user must not be the operator's own user `{name}`: every \
                 agent runs as it; name the lease listener's own user"
            )));
        }
        Ok(Self {
            name: name.to_string(),
            uid,
            gid,
        })
    }
}

fn operator_uid() -> libc::uid_t {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// `(uid, primary gid)` of `name`, or `None` when there is no such user.
fn lookup_user(name: &str) -> Result<Option<(libc::uid_t, libc::gid_t)>, OrbitError> {
    let c_name = std::ffi::CString::new(name).map_err(|_| {
        OrbitError::InvalidInput("lease.intake.peer_user must not contain a NUL byte".to_string())
    })?;
    let mut buf = vec![0 as libc::c_char; 1024];
    loop {
        // SAFETY: getpwnam_r writes only into the caller-owned `pwd` and
        // `buf`; the fields read below are copied out before either is
        // dropped, and only after a success with a non-null `result`.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwnam_r(
                c_name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == 0 {
            return Ok((!result.is_null()).then_some((pwd.pw_uid, pwd.pw_gid)));
        }
        if rc == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return Err(OrbitError::Io(format!(
            "look up lease.intake.peer_user `{name}`: {}",
            io::Error::from_raw_os_error(rc)
        )));
    }
}

/// The bound intake socket, removed when dropped.
pub(crate) struct LeaseIntakeSocket {
    /// Taken by [`LeaseIntakeSocket::serve`].
    pub(super) listener: Option<UnixListener>,
    pub(super) path: PathBuf,
    pub(super) peer: PeerUser,
    /// Device and inode of the file this process bound, so [`Drop`] never
    /// removes a socket another server bound at the same path.
    identity: Option<(u64, u64)>,
}

impl LeaseIntakeSocket {
    /// Resolve `peer_user` and bind `$ORBIT_HOME/run/lease-intake.sock`, with
    /// the directory and socket owned by the operator and group-owned by the
    /// peer user's primary group, and closed to everyone else.
    pub(crate) fn bind(orbit_home: &Path, peer_user: &str) -> Result<Self, OrbitError> {
        let peer = PeerUser::resolve(peer_user)?;
        let path = orbit_home.join(LEASE_INTAKE_SOCKET_PATH);
        let Some(dir) = path.parent() else {
            return Err(OrbitError::Execution(
                "lease intake socket path has no parent".to_string(),
            ));
        };
        prepare_run_dir(dir, &peer)?;
        remove_stale_socket(&path)?;
        let listener = UnixListener::bind(&path).map_err(|error| {
            OrbitError::Io(format!(
                "bind the lease intake socket {}: {error}",
                path.display()
            ))
        })?;
        let mut socket = Self {
            listener: Some(listener),
            path,
            peer,
            identity: None,
        };
        restrict(&socket.path, &socket.peer, SOCKET_MODE)?;
        let metadata = std::fs::symlink_metadata(&socket.path).map_err(|error| {
            OrbitError::Io(format!("inspect {}: {error}", socket.path.display()))
        })?;
        socket.identity = Some((metadata.dev(), metadata.ino()));
        Ok(socket)
    }

    /// A socket around an already-bound `listener` that admits `peer_uid`,
    /// so a test can connect as its own user, which [`Self::bind`] refuses.
    #[cfg(test)]
    pub(super) fn for_test_listener(
        listener: UnixListener,
        path: PathBuf,
        peer_uid: libc::uid_t,
    ) -> Self {
        Self {
            listener: Some(listener),
            path,
            peer: PeerUser {
                name: "lease-intake-test".to_string(),
                uid: peer_uid,
                gid: 0,
            },
            identity: None,
        }
    }

    /// Where the socket is bound.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LeaseIntakeSocket {
    fn drop(&mut self) {
        self.listener = None;
        // Only the socket this process bound: once it stops listening, another
        // server may bind the path, and that socket is not ours to remove.
        let Some(identity) = self.identity else {
            return;
        };
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.uid() == operator_uid()
            && (metadata.dev(), metadata.ino()) == identity
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn prepare_run_dir(dir: &Path, peer: &PeerUser) -> Result<(), OrbitError> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(OrbitError::InvalidInput(format!(
                    "{} must be a directory for the lease intake socket \
                     (lease.intake.peer_user)",
                    dir.display()
                )));
            }
            if metadata.uid() != operator_uid() {
                return Err(OrbitError::InvalidInput(format!(
                    "{} is owned by uid {}, not the operator; the lease intake socket \
                     (lease.intake.peer_user) needs a directory the operator owns",
                    dir.display(),
                    metadata.uid()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .mode(RUN_DIR_MODE)
                .create(dir)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect {}: {error}",
                dir.display()
            )));
        }
    }
    restrict(dir, peer, RUN_DIR_MODE)
}

/// Group `path` to the peer user's primary group and set `mode`.
fn restrict(path: &Path, peer: &PeerUser, mode: u32) -> Result<(), OrbitError> {
    std::os::unix::fs::lchown(path, None, Some(peer.gid)).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "lease.intake.peer_user `{}`: cannot give {} to its primary group (gid {}): \
             {error}; add the operator's user to that group so the socket can be shared \
             with the lease listener only",
            peer.name,
            path.display(),
            peer.gid
        ))
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", path.display())))
}

/// Remove a socket left by a previous run, only when it is a socket the
/// operator owns and nothing is serving it.
fn remove_stale_socket(path: &Path) -> Result<(), OrbitError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect {}: {error}",
                path.display()
            )));
        }
    };
    if !metadata.file_type().is_socket() || metadata.uid() != operator_uid() {
        return Err(OrbitError::InvalidInput(format!(
            "{} exists and is not a socket the operator owns; remove it before serving the \
             lease intake socket (lease.intake.peer_user)",
            path.display()
        )));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(OrbitError::InvalidInput(format!(
            "another process is serving the lease intake socket {}; stop it first",
            path.display()
        )));
    }
    std::fs::remove_file(path)
        .map_err(|error| OrbitError::Io(format!("remove stale {}: {error}", path.display())))
}
