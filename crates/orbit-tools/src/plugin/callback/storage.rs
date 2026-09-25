use super::*;

/// Count callback session records that no live child can still be using:
/// those whose recorded process is gone, and those this host's schema cannot
/// read at all.
///
/// Partially-written entries do not prevent inspecting other records, and are
/// not classified as stale because their ownership cannot be established
/// safely — see [`SessionScan`].
pub fn stale_plugin_callback_session_count(global_root: &Path) -> Result<usize, OrbitError> {
    Ok(stale_session_paths(&callback_dir(global_root))?.len())
}

pub(super) fn remove_stale_sessions(dir: &Path) -> Result<(), OrbitError> {
    for path in stale_session_paths(dir)? {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "remove stale plugin callback session `{}`: {error}",
                    path.display()
                )));
            }
        }
    }
    Ok(())
}

fn stale_session_paths(dir: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "read plugin callback session directory `{}`: {error}",
                dir.display()
            )));
        }
    };
    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        match scan_session_file(&bytes) {
            SessionScan::Current(record) => {
                if record.pid != 0 && !session_process_is_live(&record) {
                    stale.push(path);
                }
            }
            // A record this host's schema cannot read identifies nothing and
            // will never be honoured, whatever pid it names. The auth path
            // refusing it is right; leaving it here as well would strand a
            // mode-0600 file under the session directory that no surface
            // reports and no sweep removes [ORB-12879].
            SessionScan::Foreign => stale.push(path),
            SessionScan::Unreadable => {}
        }
    }
    Ok(stale)
}

/// What one file in the session directory turned out to hold.
enum SessionScan {
    /// A record this host's schema reads.
    Current(SessionRecord),
    /// A complete JSON value that is not a current-schema session record: a
    /// leftover minted by a host whose record schema differs from this one.
    /// Every byte of it was written, so it is a finished record rather than a
    /// mint caught in progress.
    Foreign,
    /// Bytes that are not a complete JSON value: corruption, or a record read
    /// between the `create_new` that opened it and the single `write` that
    /// fills it. `mint` passes through that window, so a concurrent sweep must
    /// not reap what it finds there — unlinking a live session's file would
    /// also break the Landlock grant its child reads the record through, and
    /// the descriptor the host is about to hand that child.
    Unreadable,
}

fn scan_session_file(bytes: &[u8]) -> SessionScan {
    match parse_session(bytes) {
        Some(record) => SessionScan::Current(record),
        None => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(_) => SessionScan::Foreign,
            Err(_) => SessionScan::Unreadable,
        },
    }
}

pub(super) fn session_process_is_live(record: &SessionRecord) -> bool {
    process_start_key(record.pid).is_some_and(|key| key.starttime == record.starttime)
}

pub(super) fn parse_session(bytes: &[u8]) -> Option<SessionRecord> {
    let record: SessionRecord = serde_json::from_slice(bytes).ok()?;
    (record.schema_version == SESSION_SCHEMA_VERSION
        && !record.plugin.trim().is_empty()
        && is_token_hex(&record.token))
    .then_some(record)
}

/// Create one session file, failing with `AlreadyExists` if the token is
/// taken. `create_new` is the check: the record is the child's credential, so
/// two mints must never share an inode, and the kernel decides that without a
/// window between the test and the write.
pub(super) fn write_session_exclusive(path: &Path, record: &SessionRecord) -> std::io::Result<()> {
    let bytes = session_bytes(record)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

/// Rewrite the record the child was granted, keeping its inode.
///
/// A Landlock rule binds an inode, and the child's read grant is compiled
/// from this path before the child starts. Replacing the file through a
/// temporary name and `rename` would leave the grant on an unlinked inode and
/// the record unreadable to the very process it identifies, so the bytes are
/// written back in place. They are one small `write`, which the kernel serves
/// a concurrent reader either wholly before or wholly after.
///
/// Deliberately not `O_TRUNC`: by the time the host binds the child's pid the
/// child is already running and already holds this inode open as its
/// credential. Truncating first would leave a window in which the record it
/// reads is an empty file — a refused callback for a backend that did
/// everything right. `pid` and `starttime` only ever go from zero to real
/// values, so the rewrite covers the old bytes and the `set_len` after it is
/// the correctness backstop rather than the normal case.
pub(super) fn rewrite_session_in_place(
    path: &Path,
    record: &SessionRecord,
) -> Result<(), OrbitError> {
    let bytes = session_bytes(record).map_err(|error| {
        OrbitError::Io(format!(
            "serialize plugin callback session `{}`: {error}",
            path.display()
        ))
    })?;
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.write_all(&bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()
    })();
    write_result.map_err(|error| {
        OrbitError::Io(format!(
            "update plugin callback session `{}`: {error}",
            path.display()
        ))
    })
}

pub(super) fn session_bytes(record: &SessionRecord) -> std::io::Result<Vec<u8>> {
    serde_json::to_vec(record).map_err(|error| std::io::Error::other(error.to_string()))
}

pub(super) fn random_token() -> Result<String, OrbitError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("draw plugin callback token: {error}")))?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(super) fn is_token_hex(value: &str) -> bool {
    value.len() == TOKEN_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A credential that names no live session, or a session the sandbox keeps
/// out of this caller's reach.
pub fn invalid_callback_credential(plugin: Option<&str>) -> OrbitError {
    OrbitError::PolicyDenied(match plugin {
        Some(plugin) => format!(
            "plugin '{plugin}' callback credential is missing or invalid; a backend the host \
             launched cannot reach Orbit without the host-issued session"
        ),
        None => "plugin callback credential is missing or invalid; a backend the host launched \
             cannot reach Orbit without the host-issued session"
            .to_string(),
    })
}

/// A token that resolves to a session belonging to another process.
pub fn mismatched_callback_credential(token: &str, ancestry: Option<&str>) -> OrbitError {
    OrbitError::PolicyDenied(match ancestry {
        Some(ancestry) => format!(
            "plugin callback credential for '{token}' does not match the live backend process \
             '{ancestry}'"
        ),
        None => format!(
            "plugin callback credential for '{token}' is not held by the calling process; a \
             host-issued session identifies the backend it was minted for and its descendants, \
             and is not transferable"
        ),
    })
}

/// A credential this host has stopped honouring.
pub fn retired_callback_credential(plugin: Option<&str>) -> OrbitError {
    let who = match plugin {
        Some(plugin) => format!("plugin '{plugin}' presented"),
        None => "a plugin backend presented".to_string(),
    };
    OrbitError::PolicyDenied(format!(
        "{who} only the retired callback credential; identity is the session record the host \
         hands the backend on file descriptor {PLUGIN_CALLBACK_FD}, which a backend must keep \
         open across `setsid`, `exec` and any wrapper script. Set \
         `plugin.legacy_callback_identity = true` to accept the environment token and process \
         ancestry for one more release"
    ))
}

/// A caller the plugin sandbox confines that presented no credential at all.
pub fn unidentified_plugin_child() -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "a plugin backend descendant reached Orbit without the host-issued callback session; \
         changing process group or session does not make a confined child an ordinary caller, \
         and file descriptor {PLUGIN_CALLBACK_FD} must stay open through to every process that \
         calls back"
    ))
}
