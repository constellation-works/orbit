//! Host-owned identity for a plugin backend that calls back into Orbit.
//!
//! The child controls its environment: it can unset `ORBIT_PLUGIN` or any
//! other variable the host stamped. Identity therefore lives in a session
//! file the host writes under `{global_root}/state/plugin-callbacks/`, a
//! path the `orbit_tools` sandbox does not grant for writing, plus the
//! child's pid and kernel start time. A later `orbit tool run` or MCP
//! `tools/call` presents the token *or* is recognised by process ancestry.
//! Unsetting the environment is a missing credential, not an absent
//! restriction.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::process::ancestry::{
    current_parent_pid, current_process_group, process_start_key,
};
use orbit_types::plugin::PluginProvenance;
use serde::{Deserialize, Serialize};

/// Informational namespace the backend already carries. Not the gate.
pub const ORBIT_PLUGIN_ENV: &str = "ORBIT_PLUGIN";

/// Host-issued callback token. The child may unset it; ancestry still
/// identifies the session. A presented value that matches no session is
/// a missing credential and is refused.
pub const ORBIT_PLUGIN_CALLBACK_ENV: &str = "ORBIT_PLUGIN_CALLBACK";

const SESSION_DIR: &str = "state/plugin-callbacks";
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Live callback session the host minted for one backend child.
#[derive(Debug)]
pub struct PluginCallbackSession {
    path: PathBuf,
    token: String,
    record: SessionRecord,
}

/// The plugin a resolved callback belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCallbackIdentity {
    pub name: String,
    pub version: String,
    pub manifest_digest: String,
}

impl PluginCallbackIdentity {
    pub fn provenance(&self) -> PluginProvenance {
        PluginProvenance {
            name: self.name.clone(),
            version: self.version.clone(),
            manifest_digest: self.manifest_digest.clone(),
            grants: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    schema_version: u32,
    plugin: String,
    version: String,
    manifest_digest: String,
    pid: u32,
    starttime: u64,
}

impl PluginCallbackSession {
    /// Create the session file and return a guard that unlinks it on drop.
    pub fn mint(global_root: &Path, provenance: &PluginProvenance) -> Result<Self, OrbitError> {
        let dir = callback_dir(global_root);
        fs::create_dir_all(&dir).map_err(|error| {
            OrbitError::Io(format!(
                "create plugin callback session directory `{}`: {error}",
                dir.display()
            ))
        })?;
        remove_stale_sessions(&dir)?;
        for _ in 0..8 {
            let token = random_token()?;
            let path = dir.join(&token);
            let record = SessionRecord {
                schema_version: 1,
                plugin: provenance.name.clone(),
                version: provenance.version.clone(),
                manifest_digest: provenance.manifest_digest.clone(),
                pid: 0,
                starttime: 0,
            };
            match write_session_exclusive(&path, &record) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        token,
                        record,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(OrbitError::Io(format!(
                        "write plugin callback session `{}`: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(OrbitError::Execution(
            "could not allocate a unique plugin callback token".to_string(),
        ))
    }

    /// Stamp the token into the cleared-and-set child environment.
    pub fn stamp_env(&self, env: &mut Vec<(String, String)>) {
        upsert_env(env, ORBIT_PLUGIN_CALLBACK_ENV, self.token.clone());
    }

    /// The token stamped into the child. Tests use this to present or clear it.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Record the spawned child's pid and start time so ancestry can find it
    /// after the child unsets its environment.
    pub fn bind_pid(&mut self, pid: u32) -> Result<(), OrbitError> {
        let key = process_start_key(pid).ok_or_else(|| {
            OrbitError::Execution(format!(
                "plugin callback session could not read start time for pid {pid}"
            ))
        })?;
        self.record.pid = key.pid;
        self.record.starttime = key.starttime;
        rewrite_session(&self.path, &self.record)
    }
}

impl Drop for PluginCallbackSession {
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    path = %self.path.display(),
                    "could not remove plugin callback session: {error}"
                );
            }
        }
    }
}

/// Identify a plugin callback from the host-issued token or from process
/// ancestry against live session files. `None` is an ordinary caller.
///
/// A token that is set and matches no session is a missing credential:
/// [`OrbitError::PolicyDenied`], never an absent restriction. Ancestry
/// still names the plugin so the refusal can be audited.
pub fn resolve_plugin_callback(
    global_root: &Path,
) -> Result<Option<PluginCallbackIdentity>, OrbitError> {
    match resolve_plugin_callback_session(global_root)? {
        CallbackResolution::None => Ok(None),
        CallbackResolution::Identified(identity) => Ok(Some(identity)),
        CallbackResolution::InvalidCredential(identity) => Err(invalid_callback_credential(
            identity.as_ref().map(|id| id.name.as_str()),
        )),
        CallbackResolution::Mismatched { token, ancestry } => {
            Err(OrbitError::PolicyDenied(format!(
                "plugin callback credential for '{token}' does not match the live backend process '{ancestry}'"
            )))
        }
    }
}

/// Full resolution used by dispatch so an invalid credential can still stamp
/// plugin identity on the audit row.
pub fn resolve_plugin_callback_session(
    global_root: &Path,
) -> Result<CallbackResolution, OrbitError> {
    let dir = callback_dir(global_root);
    let token = std::env::var(ORBIT_PLUGIN_CALLBACK_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let token_record = match token.as_deref() {
        Some(value) => match load_token_session(&dir, value) {
            Ok(record) => TokenLookup::Found(record),
            Err(OrbitError::PolicyDenied(_)) => TokenLookup::Invalid,
            Err(error) => return Err(error),
        },
        None => TokenLookup::Absent,
    };
    let ancestry_record = find_ancestry_session(&dir);
    Ok(match (token_record, ancestry_record) {
        (TokenLookup::Found(token), Some(ancestry)) if token.plugin != ancestry.plugin => {
            CallbackResolution::Mismatched {
                token: token.plugin,
                ancestry: ancestry.plugin,
            }
        }
        (TokenLookup::Found(record), _) => CallbackResolution::Identified(identity_from(&record)),
        (TokenLookup::Invalid, ancestry) => {
            CallbackResolution::InvalidCredential(ancestry.as_ref().map(identity_from))
        }
        (TokenLookup::Absent, Some(record)) => {
            CallbackResolution::Identified(identity_from(&record))
        }
        (TokenLookup::Absent, None) => CallbackResolution::None,
    })
}

/// Outcome of looking up a live plugin-callback session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackResolution {
    /// Ordinary caller: no token, no ancestor session.
    None,
    /// Host-launched backend, identified by token and/or ancestry.
    Identified(PluginCallbackIdentity),
    /// A token was presented that matches no session. `Some` when ancestry
    /// still names the backend so the refusal can be audited.
    InvalidCredential(Option<PluginCallbackIdentity>),
    /// Token and ancestry name two different plugins.
    Mismatched { token: String, ancestry: String },
}

enum TokenLookup {
    Absent,
    Found(SessionRecord),
    Invalid,
}

fn callback_dir(global_root: &Path) -> PathBuf {
    global_root.join(SESSION_DIR)
}

fn identity_from(record: &SessionRecord) -> PluginCallbackIdentity {
    PluginCallbackIdentity {
        name: record.plugin.clone(),
        version: record.version.clone(),
        manifest_digest: record.manifest_digest.clone(),
    }
}

fn load_token_session(dir: &Path, token: &str) -> Result<SessionRecord, OrbitError> {
    if !is_token_hex(token) {
        return Err(invalid_callback_credential(None));
    }
    let path = dir.join(token);
    match fs::read(&path) {
        Ok(bytes) => parse_session(&bytes).ok_or_else(|| invalid_callback_credential(None)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(invalid_callback_credential(None))
        }
        Err(error) => Err(OrbitError::Io(format!(
            "read plugin callback session `{}`: {error}",
            path.display()
        ))),
    }
}

fn find_ancestry_session(dir: &Path) -> Option<SessionRecord> {
    // Landlock refuses `/proc/<pid>` of any other process (it would leak
    // `environ`). Identity therefore uses syscalls that still work in the
    // confined child: this pid, `getppid`, and `getpgrp`. Orbit spawns the
    // backend with `process_group(0)`, so PGID equals the backend pid and
    // every descendant inherits it — including `orbit tool run` started from
    // a `$()` subshell.
    let self_pid = std::process::id();
    let parent_pid = current_parent_pid();
    let pgid = current_process_group();
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        let Some(record) = parse_session(&bytes) else {
            continue;
        };
        if record.pid == 0 {
            continue;
        }
        if !session_process_is_live(&record) {
            continue;
        }
        if record.pid == self_pid || parent_pid == Some(record.pid) || pgid == Some(record.pid) {
            return Some(record);
        }
    }
    None
}

/// Count callback session records whose recorded process no longer exists.
///
/// Corrupt or partially-written entries do not prevent inspecting other
/// records. They are not classified as stale because their ownership cannot
/// be established safely.
pub fn stale_plugin_callback_session_count(global_root: &Path) -> Result<usize, OrbitError> {
    Ok(stale_session_paths(&callback_dir(global_root))?.len())
}

fn remove_stale_sessions(dir: &Path) -> Result<(), OrbitError> {
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
        let Some(record) = parse_session(&bytes) else {
            continue;
        };
        if record.pid != 0 && !session_process_is_live(&record) {
            stale.push(path);
        }
    }
    Ok(stale)
}

fn session_process_is_live(record: &SessionRecord) -> bool {
    process_start_key(record.pid).is_some_and(|key| key.starttime == record.starttime)
}

fn parse_session(bytes: &[u8]) -> Option<SessionRecord> {
    let record: SessionRecord = serde_json::from_slice(bytes).ok()?;
    (record.schema_version == 1 && !record.plugin.trim().is_empty()).then_some(record)
}

fn write_session_exclusive(path: &Path, record: &SessionRecord) -> std::io::Result<()> {
    if path.try_exists()? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "plugin callback session already exists",
        ));
    }
    let bytes = session_bytes(record)?;
    write_session_atomically(path, &bytes).map_err(|error| std::io::Error::other(error.to_string()))
}

fn rewrite_session(path: &Path, record: &SessionRecord) -> Result<(), OrbitError> {
    let bytes = session_bytes(record).map_err(|error| {
        OrbitError::Io(format!(
            "serialize plugin callback session `{}`: {error}",
            path.display()
        ))
    })?;
    write_session_atomically(path, &bytes)
}

fn write_session_atomically(path: &Path, bytes: &[u8]) -> Result<(), OrbitError> {
    let temp = path.with_extension(format!("tmp-{}", random_token()?));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(OrbitError::Io(format!(
            "update plugin callback session `{}`: {error}",
            path.display()
        )));
    }
    fs::rename(&temp, path).map_err(|error| {
        OrbitError::Io(format!(
            "update plugin callback session `{}`: {error}",
            path.display()
        ))
    })
}

fn session_bytes(record: &SessionRecord) -> std::io::Result<Vec<u8>> {
    serde_json::to_vec(record).map_err(|error| std::io::Error::other(error.to_string()))
}

fn random_token() -> Result<String, OrbitError> {
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

fn is_token_hex(value: &str) -> bool {
    value.len() == TOKEN_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_callback_credential(plugin: Option<&str>) -> OrbitError {
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

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env.push((key.to_string(), value));
    }
}
