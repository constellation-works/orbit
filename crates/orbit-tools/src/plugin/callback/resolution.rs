use super::*;

/// Identify a plugin callback from the inherited host-issued descriptor.
/// `None` is an ordinary caller. Token-only callers, live backend descendants
/// without the descriptor, and unidentified confined children are refused.
pub fn resolve_plugin_callback(
    global_root: &Path,
) -> Result<Option<PluginCallbackIdentity>, OrbitError> {
    match resolve_plugin_callback_session(global_root)? {
        CallbackResolution::None => Ok(None),
        CallbackResolution::Identified(identity) => Ok(Some(identity)),
        CallbackResolution::RetiredCredential(identity) => Err(retired_callback_credential(
            identity.as_ref().map(|id| id.provenance.name.as_str()),
        )),
        CallbackResolution::UnidentifiedPluginChild => Err(unidentified_plugin_child()),
    }
}

/// Full resolution used by dispatch so a missing descriptor can still stamp
/// plugin identity on the audit row when live ancestry names the backend.
pub fn resolve_plugin_callback_session(
    global_root: &Path,
) -> Result<CallbackResolution, OrbitError> {
    let dir = callback_dir(global_root);
    // Only the host-issued descriptor authorizes a callback. It survives
    // `setsid` and a cleared environment without relying on process ancestry.
    if let Some(record) = credential_from_descriptor(&dir) {
        return Ok(CallbackResolution::Identified(identity_from(&record)));
    }
    let has_retired_token = std::env::var(ORBIT_PLUGIN_CALLBACK_ENV)
        .ok()
        .is_some_and(|value| !value.trim().is_empty());
    let ancestry = scan_ancestry_session(global_root, &CallerProcess::current());
    // Retain the default refusal for a backend that shed its descriptor,
    // including unsandboxed callers with a readable session directory.
    // The retired environment token is only evidence to refuse, never a
    // credential to load or authorize.
    if has_retired_token || ancestry.session().is_some() {
        return Ok(CallbackResolution::RetiredCredential(
            ancestry.session().map(identity_from),
        ));
    }
    Ok(match ancestry {
        AncestryScan::Unreadable => CallbackResolution::UnidentifiedPluginChild,
        AncestryScan::None | AncestryScan::Session(_) => CallbackResolution::None,
    })
}

/// Outcome of looking up a live plugin-callback session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackResolution {
    /// Ordinary caller: no token, no ancestor session.
    None,
    /// Host-launched backend, identified by its host-issued descriptor.
    Identified(PluginCallbackIdentity),
    /// No valid descriptor, but a retired token or live ancestry marks the
    /// caller as a backend. `Some` names the backend for refusal auditing.
    RetiredCredential(Option<PluginCallbackIdentity>),
    /// No credential, from a process that cannot read the host-owned session
    /// directory at all. Only a plugin sandbox denies that read, so identity
    /// is missing rather than irrelevant.
    UnidentifiedPluginChild,
}

/// What a scan of the session directory could see from the calling process.
enum AncestryScan {
    /// A live session this process belongs to.
    Session(SessionRecord),
    /// The directory is readable and names no session for this process.
    None,
    /// The directory cannot be read: the caller is inside a plugin sandbox.
    Unreadable,
}

impl AncestryScan {
    fn session(&self) -> Option<&SessionRecord> {
        match self {
            Self::Session(record) => Some(record),
            Self::None | Self::Unreadable => None,
        }
    }
}

/// The pids a confined child can still ask the kernel about: its own, its
/// parent's, and its process group's. Orbit spawns a backend with
/// `process_group(0)`, so an ordinary descendant carries the backend pid as
/// its PGID; one that called `setsid` or `setpgid` carries none of the three
/// and is therefore not the process any record was bound to.
struct CallerProcess {
    self_pid: u32,
    parent_pid: Option<u32>,
    pgid: Option<u32>,
}

impl CallerProcess {
    fn current() -> Self {
        Self {
            self_pid: std::process::id(),
            parent_pid: current_parent_pid(),
            pgid: current_process_group(),
        }
    }

    /// Whether a live record matches this process, its parent, or its group.
    /// The scan checks start time before using this match; ancestry can only
    /// cause a refusal, never authorize a callback without its descriptor.
    fn owns(&self, record: &SessionRecord) -> bool {
        record.pid == self.self_pid
            || self.parent_pid == Some(record.pid)
            || self.pgid == Some(record.pid)
    }
}

/// Open the host's own read handle on a freshly written record.
///
/// The handle is deliberately kept clear of [`PLUGIN_CALLBACK_FD`] itself. A
/// long-lived host — `orbit mcp serve`, a clock tick — holds one of these per
/// live backend *and* resolves callbacks of its own; a record sitting on the
/// number the resolver inspects would identify the host as the plugin it
/// spawned. `try_clone` hands back the lowest free descriptor, so cloning
/// until the number clears the standard streams and the callback number
/// settles the question without a `fcntl` of our own; the low descriptors are
/// closed when the discarded handles drop.
#[cfg(unix)]
pub(super) fn open_credential(path: &Path) -> Result<File, OrbitError> {
    use std::os::fd::AsRawFd;

    let open = |file: File| -> std::io::Result<File> {
        let mut low = Vec::new();
        let mut file = file;
        while file.as_raw_fd() <= PLUGIN_CALLBACK_FD {
            let next = file.try_clone()?;
            low.push(file);
            file = next;
        }
        Ok(file)
    };
    File::open(path).and_then(open).map_err(|error| {
        OrbitError::Io(format!(
            "open plugin callback session `{}`: {error}",
            path.display()
        ))
    })
}

/// The credential travels to the child as an inherited descriptor, which
/// only Unix spawns carry; refusing here keeps a session from being minted
/// for a child that could never present it.
#[cfg(not(unix))]
pub(super) fn open_credential(path: &Path) -> Result<File, OrbitError> {
    Err(OrbitError::Execution(format!(
        "plugin callback session `{}`: callback credentials are passed on an \
         inherited descriptor, which is unsupported on this platform",
        path.display()
    )))
}

/// The number the credential is expected on: [`PLUGIN_CALLBACK_FD`], or what
/// [`ORBIT_PLUGIN_CALLBACK_FD_ENV`] names. The standard streams are never
/// accepted — a caller with a redirected stdin is not presenting a credential.
fn callback_descriptor() -> i32 {
    std::env::var(ORBIT_PLUGIN_CALLBACK_FD_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|fd| *fd >= PLUGIN_CALLBACK_FD)
        .unwrap_or(PLUGIN_CALLBACK_FD)
}

/// The session record the caller holds open, when it really holds one.
///
/// Absent — never a refusal — for anything that is not this host's own
/// record: an ordinary caller may have any file on the number, and reading
/// one is not a claim to be a plugin. The refusal for a backend that dropped
/// its credential comes from the sandbox probe instead, which is what tells a
/// confined child apart from a local caller.
#[cfg(unix)]
fn credential_from_descriptor(dir: &Path) -> Option<SessionRecord> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::{FileExt, MetadataExt};

    let fd = callback_descriptor();
    // SAFETY: the descriptor is only borrowed. `ManuallyDrop` keeps the
    // wrapper from closing a number this process does not own, and a number
    // that is closed or was never opened fails every call below with `EBADF`,
    // which reads as "no credential".
    let file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
    let held = file.metadata().ok()?;
    if !held.is_file() {
        return None;
    }
    // Positional, so the caller's own file offset is left where it was: this
    // process may be reading the very same descriptor for its own reasons.
    let mut bytes = vec![0u8; MAX_SESSION_BYTES];
    let read = file.read_at(&mut bytes, 0).ok()?;
    bytes.truncate(read);
    let record = parse_session(&bytes)?;
    // The record names itself, and only the host can put a file under that
    // name: a plugin may write neither the session directory nor any record in
    // it. A descriptor on a record the host wrote therefore resolves to the
    // same inode by name; a forgery does not.
    let named = orbit_common::fs::io::open_read_only_no_follow(&dir.join(&record.token)).ok()?;
    let named = named.metadata().ok()?;
    (named.is_file() && named.dev() == held.dev() && named.ino() == held.ino()).then_some(record)
}

/// Descriptor inheritance is a Unix contract, and so is every sandbox Orbit
/// runs a plugin under.
#[cfg(not(unix))]
fn credential_from_descriptor(_dir: &Path) -> Option<SessionRecord> {
    None
}

pub(super) fn callback_dir(global_root: &Path) -> PathBuf {
    global_root.join(SESSION_DIR)
}

/// Resolve the existing callback directory under the selected host root.
/// Symlinked state subdirectories that escape that root are refused before a
/// directory scan can follow them.
fn validated_callback_session_entries(global_root: &Path) -> std::io::Result<fs::ReadDir> {
    let root = global_root.canonicalize()?;
    let dir = callback_dir(&root).canonicalize()?;
    if !dir.starts_with(&root) || !std::fs::metadata(&dir)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin callback directory is outside its host root or is not a directory",
        ));
    }
    fs::read_dir(&dir)
}

fn identity_from(record: &SessionRecord) -> PluginCallbackIdentity {
    PluginCallbackIdentity {
        provenance: PluginProvenance {
            name: record.plugin.clone(),
            version: record.version.clone(),
            manifest_digest: record.manifest_digest.clone(),
            grants: Vec::new(),
        },
        effective_tools: record.effective_tools.clone(),
    }
}

/// Sorted and deduped, so the recorded ceiling does not depend on the order
/// the caller happened to list its own allowlist in.
pub(super) fn normalize_tools(tools: &[String]) -> Vec<String> {
    let mut tools = tools.to_vec();
    tools.sort();
    tools.dedup();
    tools
}

fn scan_ancestry_session(global_root: &Path, caller: &CallerProcess) -> AncestryScan {
    // Landlock refuses `/proc/<pid>` of any other process (it would leak
    // `environ`). Identity therefore uses syscalls that still work in the
    // confined child: this pid, `getppid`, and `getpgrp`.
    //
    // A confined backend is not granted the session directory at all, so this
    // scan is how an unidentified plugin child is told apart from a local
    // caller: `EACCES` here is the sandbox answering, and an absent directory
    // means no session was ever minted on this host.
    let entries = match validated_callback_session_entries(global_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return AncestryScan::Unreadable;
        }
        Err(_) => return AncestryScan::None,
    };
    for entry in entries.flatten() {
        let Ok(file) = orbit_common::fs::io::open_read_only_no_follow(&entry.path()) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_SESSION_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() > MAX_SESSION_BYTES
        {
            continue;
        }
        let Some(record) = parse_session(&bytes) else {
            continue;
        };
        if record.pid == 0 || !session_process_is_live(&record) {
            continue;
        }
        if caller.owns(&record) {
            return AncestryScan::Session(record);
        }
    }
    AncestryScan::None
}
