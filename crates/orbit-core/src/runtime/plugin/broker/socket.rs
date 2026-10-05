//! Where a run's broker socket lives (design §4.1).
//!
//! Each run gets its own `0700` directory,
//! `<global_root>/state/plugin-broker/<token>/`, holding `broker.sock` and an
//! `owner` file naming the host process. The token is random only to avoid
//! collisions; it is not a credential. `state/plugin-broker/` is host-owned
//! `0700` and appears in no agent write grant, so no agent can replace or
//! unlink another run's socket.
//!
//! The path is refused rather than moved when it does not fit `sun_path`:
//! Bubblewrap gives the agent a private `/tmp`, and macOS agents can write the
//! shared temporary roots.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::process::ancestry::{ProcessStartKey, process_start_key};

/// The broker root, relative to the global root.
pub(crate) const BROKER_DIR: &str = "state/plugin-broker";
/// The socket's name inside a run's directory.
const SOCKET_NAME: &str = "broker.sock";
/// The file naming the host process that owns a run's directory.
const OWNER_NAME: &str = "owner";
/// Owner-only mode for the broker root and every run directory.
const DIR_MODE: u32 = 0o700;

/// One run's socket directory. [`Self::remove`] deletes it.
#[derive(Debug)]
pub(crate) struct RunSocketDir {
    dir: PathBuf,
    socket: PathBuf,
}

impl RunSocketDir {
    /// Create a fresh run directory under `global_root`.
    ///
    /// The socket path's length is checked before anything is created, and
    /// each directory on the way is created without following links, then
    /// re-inspected: a symlink standing in for `state` or `state/plugin-broker`
    /// is refused, as is a broker root another user owns.
    pub(crate) fn create(global_root: &Path) -> Result<Self, OrbitError> {
        let root = trusted_global_root(global_root)?;
        let token = random_token()?;
        let broker_root = root.join(BROKER_DIR);
        let dir = broker_root.join(&token);
        let socket = dir.join(SOCKET_NAME);
        check_socket_path_length(&socket)?;

        create_broker_root(&root)?;
        sweep_orphaned_in(&broker_root);
        fs::DirBuilder::new()
            .mode(DIR_MODE)
            .create(&dir)
            .map_err(|error| path_error("create plugin broker run directory", &dir, error))?;
        let created = Self { dir, socket };
        if let Some(owner) = process_start_key(std::process::id()) {
            fs::write(
                created.dir.join(OWNER_NAME),
                format!("{} {}\n", owner.pid, owner.starttime),
            )
            .map_err(|error| {
                let _ = created.remove();
                path_error("record plugin broker owner", &created.dir, error)
            })?;
        }
        Ok(created)
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Remove the socket, the owner file and the directory. Anything else
    /// found inside is left in place and reported.
    pub(crate) fn remove(&self) -> io::Result<()> {
        remove_run_dir(&self.dir)
    }
}

/// Remove run directories whose owning host process is gone.
///
/// A host that exits normally removes its own directory. One that was killed
/// (a cancelled run's worker receives SIGTERM, then SIGKILL) cannot, so the
/// canceller and every later broker start sweep here. A directory with no
/// readable owner is left alone: it is either being created right now or not
/// Orbit's to remove.
pub(crate) fn sweep_orphaned(global_root: &Path) {
    let Ok(root) = trusted_global_root(global_root) else {
        return;
    };
    let broker_root = root.join(BROKER_DIR);
    let is_real_dir = fs::symlink_metadata(root.join("state"))
        .is_ok_and(|metadata| metadata.is_dir())
        && fs::symlink_metadata(&broker_root).is_ok_and(|metadata| metadata.is_dir());
    if is_real_dir {
        sweep_orphaned_in(&broker_root);
    }
}

fn sweep_orphaned_in(broker_root: &Path) {
    let Ok(entries) = fs::read_dir(broker_root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !fs::symlink_metadata(&dir).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        let Some(owner) = read_owner(&dir) else {
            continue;
        };
        if process_start_key(owner.pid) == Some(owner) {
            continue;
        }
        if let Err(error) = remove_run_dir(&dir) {
            tracing::warn!(
                target: "orbit.plugin_broker",
                dir = %dir.display(),
                owner_pid = owner.pid,
                error = %error,
                "could not remove an orphaned plugin broker directory"
            );
        }
    }
}

fn read_owner(dir: &Path) -> Option<ProcessStartKey> {
    let path = dir.join(OWNER_NAME);
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let mut fields = text.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let starttime = fields.next()?.parse().ok()?;
    Some(ProcessStartKey { pid, starttime })
}

fn remove_run_dir(dir: &Path) -> io::Result<()> {
    for name in [SOCKET_NAME, OWNER_NAME] {
        match fs::remove_file(dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match fs::remove_dir(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The configured global root, resolved without writing anything. Aliasing in
/// the root itself is the operator's layout; everything beneath it is checked.
fn trusted_global_root(global_root: &Path) -> Result<PathBuf, OrbitError> {
    if !global_root.is_absolute()
        || !global_root
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(OrbitError::InvalidInput(format!(
            "plugin broker root `{}` must be an absolute path without traversal",
            global_root.display()
        )));
    }
    let root = global_root
        .canonicalize()
        .map_err(|error| path_error("resolve plugin broker root", global_root, error))?;
    if !root.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin broker root `{}` is not a directory",
            global_root.display()
        )));
    }
    Ok(root)
}

/// Create `state/plugin-broker` one component at a time under `root`, never
/// writing through an existing link.
fn create_broker_root(root: &Path) -> Result<PathBuf, OrbitError> {
    let mut dir = root.to_path_buf();
    for component in Path::new(BROKER_DIR).components() {
        dir.push(component);
        match fs::DirBuilder::new().mode(DIR_MODE).create(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(path_error("create plugin broker root", &dir, error)),
        }
        let metadata = fs::symlink_metadata(&dir)
            .map_err(|error| path_error("inspect plugin broker root", &dir, error))?;
        if metadata.file_type().is_symlink() {
            return Err(OrbitError::PolicyDenied(format!(
                "plugin broker refuses symlinked directory `{}`",
                dir.display()
            )));
        }
        if !metadata.is_dir() {
            return Err(OrbitError::PolicyDenied(format!(
                "plugin broker refuses non-directory path `{}`",
                dir.display()
            )));
        }
    }
    let metadata = fs::symlink_metadata(&dir)
        .map_err(|error| path_error("inspect plugin broker root", &dir, error))?;
    // SAFETY: `geteuid` only reads the calling process's credentials.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() != euid {
        return Err(OrbitError::PolicyDenied(format!(
            "plugin broker root `{}` is owned by uid {}, not this host (uid {euid})",
            dir.display(),
            metadata.uid()
        )));
    }
    if metadata.mode() & 0o777 != DIR_MODE {
        fs::set_permissions(&dir, fs::Permissions::from_mode(DIR_MODE))
            .map_err(|error| path_error("restrict plugin broker root", &dir, error))?;
    }
    Ok(dir)
}

/// Refuse a socket path that does not fit `sockaddr_un.sun_path` with its
/// terminating NUL (107 bytes on Linux, 103 on macOS).
fn check_socket_path_length(socket: &Path) -> Result<(), OrbitError> {
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: `sockaddr_un` is plain data; all-zero is a valid value.
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let limit = address.sun_path.len() - 1;
    let length = socket.as_os_str().as_bytes().len();
    if length > limit {
        return Err(OrbitError::InvalidInput(format!(
            "plugin broker socket path `{}` is {length} bytes; sun_path allows {limit}. \
             Use a shorter global root; the broker never moves its socket to a temporary \
             directory",
            socket.display()
        )));
    }
    Ok(())
}

fn random_token() -> Result<String, OrbitError> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| {
        OrbitError::Execution(format!("draw plugin broker socket token: {error}"))
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn path_error(action: &str, path: &Path, error: io::Error) -> OrbitError {
    OrbitError::Execution(format!("{action} `{}`: {error}", path.display()))
}
