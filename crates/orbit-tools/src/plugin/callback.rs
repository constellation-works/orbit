//! Host-owned identity for a plugin backend that calls back into Orbit.
//!
//! The child controls its environment: it can unset `ORBIT_PLUGIN` or any
//! other variable the host stamped. Identity therefore lives in a session
//! file the host writes under `{global_root}/state/plugin-callbacks/`, plus
//! the child's pid and kernel start time. A later `orbit tool run` or MCP
//! `tools/call` presents the token, and the record it names must belong to
//! the process presenting it — self, parent, or process group.
//!
//! The sandbox grants a confined backend read access to its *own* session
//! record and to nothing else in that directory (design
//! `docs/design/plugins/1_scope.md` §4.3). That is also what makes an
//! unidentified child recognisable: a process that cannot even list the
//! session directory is inside a plugin sandbox, so a missing credential
//! there is a refusal rather than an ordinary caller. Changing process group
//! or starting a new session does not change that answer — `setsid` sheds
//! ancestry, not confinement.
//!
//! The record carries *authority* as well as identity: the effective tool
//! ceiling the spawning caller had when the host minted it. Knowing which
//! plugin is calling is not enough to decide a callback, because the same
//! plugin is reachable from callers with different allowlists — the
//! intersection is computed per call and the manifest list is not it
//! [ORB-12801].

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

/// Host-issued callback token, stamped into the backend child and inherited
/// by its descendants. A value that matches no session — or that names a
/// session belonging to another process — is a missing credential and is
/// refused. Dropping it inside the sandbox is a refusal too, never a way out
/// of the plugin's allowlist.
pub const ORBIT_PLUGIN_CALLBACK_ENV: &str = "ORBIT_PLUGIN_CALLBACK";

const SESSION_DIR: &str = "state/plugin-callbacks";
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Records carry the session's effective tool ceiling from version 2 on. A
/// record without one states no ceiling, so it is not parsed at all rather
/// than read as an unbounded session: these files live only as long as the
/// child they identify, and refusing a leftover from an older host is the
/// fail-closed half of the choice.
const SESSION_SCHEMA_VERSION: u32 = 2;

/// Live callback session the host minted for one backend child.
#[derive(Debug)]
pub struct PluginCallbackSession {
    path: PathBuf,
    token: String,
    record: SessionRecord,
}

/// The plugin a resolved callback belongs to, and what that session may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCallbackIdentity {
    pub name: String,
    pub version: String,
    pub manifest_digest: String,
    /// The tool ceiling the host minted this session with: the spawning
    /// caller's own `permissions.orbit_tools` ∩ grant ∩ `allowed_tools`
    /// intersection, sorted and deduped. A callback may never reach past it,
    /// whatever the plugin row says later [ORB-12801].
    pub effective_tools: Vec<String>,
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

    /// Whether this session's ceiling still names `tool`.
    pub fn ceiling_admits(&self, tool: &str) -> bool {
        self.effective_tools.iter().any(|allowed| allowed == tool)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    schema_version: u32,
    plugin: String,
    version: String,
    manifest_digest: String,
    /// Host-owned authority, fixed at mint time. The child can rewrite
    /// `ORBIT_ALLOWED_TOOLS` and `ORBIT_ACTIVITY_TOOLS` in its own
    /// environment; it cannot write this directory at all (§4.3).
    effective_tools: Vec<String>,
    pid: u32,
    starttime: u64,
}

impl PluginCallbackSession {
    /// Create the session file and return a guard that unlinks it on drop.
    ///
    /// `effective_tools` is the spawning caller's intersection — what this
    /// one child is allowed to call back for, for as long as it lives.
    pub fn mint(
        global_root: &Path,
        provenance: &PluginProvenance,
        effective_tools: &[String],
    ) -> Result<Self, OrbitError> {
        let effective_tools = normalize_tools(effective_tools);
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
                schema_version: SESSION_SCHEMA_VERSION,
                plugin: provenance.name.clone(),
                version: provenance.version.clone(),
                manifest_digest: provenance.manifest_digest.clone(),
                effective_tools: effective_tools.clone(),
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

    /// The ceiling this session was minted with, as it was recorded.
    pub fn effective_tools(&self) -> &[String] {
        &self.record.effective_tools
    }

    /// The record file the confined child is granted read access to. A
    /// Landlock rule binds an inode, so this path is resolved once, before
    /// the child starts, and [`Self::bind_pid`] never replaces it.
    pub fn path(&self) -> &Path {
        &self.path
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
        rewrite_session_in_place(&self.path, &self.record)
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

/// Identify a plugin callback from the host-issued token, or — where the
/// session directory is readable — from process ancestry against the live
/// session files. `None` is an ordinary caller.
///
/// Every other outcome is [`OrbitError::PolicyDenied`], never an absent
/// restriction: a token matching no session, a token belonging to another
/// process, and a confined child carrying no credential at all. Ancestry
/// still names the plugin, where it can, so the refusal can be audited.
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
            Err(mismatched_callback_credential(&token, ancestry.as_deref()))
        }
        CallbackResolution::UnidentifiedPluginChild => Err(unidentified_plugin_child()),
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
    let caller = CallerProcess::current();
    let ancestry = scan_ancestry_session(&dir, &caller);
    Ok(match (token_record, ancestry) {
        // A record the presenting process is no part of is somebody else's
        // credential, whether it was read out of the session directory, copied
        // from another child's environment, or kept across a `setsid`.
        (TokenLookup::Found(record), ancestry) if !caller.owns(&record) => {
            CallbackResolution::Mismatched {
                token: record.plugin,
                ancestry: match ancestry {
                    AncestryScan::Session(ancestry) => Some(ancestry.plugin),
                    AncestryScan::None | AncestryScan::Unreadable => None,
                },
            }
        }
        (TokenLookup::Found(token), AncestryScan::Session(ancestry))
            if token.plugin != ancestry.plugin =>
        {
            CallbackResolution::Mismatched {
                token: token.plugin,
                ancestry: Some(ancestry.plugin),
            }
        }
        (TokenLookup::Found(record), _) => CallbackResolution::Identified(identity_from(&record)),
        (TokenLookup::Invalid, ancestry) => {
            CallbackResolution::InvalidCredential(ancestry.session().map(identity_from))
        }
        (TokenLookup::Absent, AncestryScan::Session(record)) => {
            CallbackResolution::Identified(identity_from(&record))
        }
        // No credential, and the host-owned session directory is out of
        // reach: only a sandboxed plugin child is refused that read, so this
        // is a backend descendant that shed its identity, not a local caller.
        (TokenLookup::Absent, AncestryScan::Unreadable) => {
            CallbackResolution::UnidentifiedPluginChild
        }
        (TokenLookup::Absent, AncestryScan::None) => CallbackResolution::None,
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
    /// The presented token is not this process's own: it names a plugin the
    /// caller is no part of, or two different plugins at once. `ancestry` is
    /// the plugin a live session still names for this process, when there is
    /// one to name.
    Mismatched {
        token: String,
        ancestry: Option<String>,
    },
    /// No credential, from a process that cannot read the host-owned session
    /// directory at all. Only a plugin sandbox denies that read, so identity
    /// is missing rather than irrelevant.
    UnidentifiedPluginChild,
}

enum TokenLookup {
    Absent,
    Found(SessionRecord),
    Invalid,
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

    /// Whether `record` was bound to this process, its parent, or its process
    /// group. A record minted but not yet bound (`pid == 0`) exists only
    /// between `mint` and `bind_pid`, before any child has run.
    ///
    /// The recorded start time is deliberately *not* consulted here. Reading
    /// `/proc/<pid>` of another process is exactly what the sandbox refuses a
    /// confined child, so a backend descendant cannot prove its own parent is
    /// alive — asking would refuse every legitimate callback. It is not
    /// needed either: this record was found through a 256-bit token the host
    /// minted for it, not by scanning for a matching pid, and
    /// [`scan_ancestry_session`] — which does scan — still checks liveness
    /// before it trusts a pid.
    fn owns(&self, record: &SessionRecord) -> bool {
        record.pid == 0
            || record.pid == self.self_pid
            || self.parent_pid == Some(record.pid)
            || self.pgid == Some(record.pid)
    }
}

fn callback_dir(global_root: &Path) -> PathBuf {
    global_root.join(SESSION_DIR)
}

fn identity_from(record: &SessionRecord) -> PluginCallbackIdentity {
    PluginCallbackIdentity {
        name: record.plugin.clone(),
        version: record.version.clone(),
        manifest_digest: record.manifest_digest.clone(),
        effective_tools: record.effective_tools.clone(),
    }
}

/// Sorted and deduped, so the recorded ceiling does not depend on the order
/// the caller happened to list its own allowlist in.
fn normalize_tools(tools: &[String]) -> Vec<String> {
    let mut tools = tools.to_vec();
    tools.sort();
    tools.dedup();
    tools
}

fn load_token_session(dir: &Path, token: &str) -> Result<SessionRecord, OrbitError> {
    if !is_token_hex(token) {
        return Err(invalid_callback_credential(None));
    }
    let path = dir.join(token);
    match fs::read(&path) {
        Ok(bytes) => parse_session(&bytes).ok_or_else(|| invalid_callback_credential(None)),
        // `NotFound` is a token that matches no session. `PermissionDenied`
        // is a confined child reaching for a record the sandbox grants some
        // other plugin: both are a credential this caller does not hold.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Err(invalid_callback_credential(None))
        }
        Err(error) => Err(OrbitError::Io(format!(
            "read plugin callback session `{}`: {error}",
            path.display()
        ))),
    }
}

fn scan_ancestry_session(dir: &Path, caller: &CallerProcess) -> AncestryScan {
    // Landlock refuses `/proc/<pid>` of any other process (it would leak
    // `environ`). Identity therefore uses syscalls that still work in the
    // confined child: this pid, `getppid`, and `getpgrp`.
    //
    // A confined backend is not granted the session directory at all, so this
    // scan is how an unidentified plugin child is told apart from a local
    // caller: `EACCES` here is the sandbox answering, and an absent directory
    // means no session was ever minted on this host.
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return AncestryScan::Unreadable;
        }
        Err(_) => return AncestryScan::None,
    };
    for entry in entries.flatten() {
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
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
    (record.schema_version == SESSION_SCHEMA_VERSION && !record.plugin.trim().is_empty())
        .then_some(record)
}

/// Create one session file, failing with `AlreadyExists` if the token is
/// taken. `create_new` is the check: the record is the child's credential, so
/// two mints must never share an inode, and the kernel decides that without a
/// window between the test and the write.
fn write_session_exclusive(path: &Path, record: &SessionRecord) -> std::io::Result<()> {
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
fn rewrite_session_in_place(path: &Path, record: &SessionRecord) -> Result<(), OrbitError> {
    let bytes = session_bytes(record).map_err(|error| {
        OrbitError::Io(format!(
            "serialize plugin callback session `{}`: {error}",
            path.display()
        ))
    })?;
    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })();
    write_result.map_err(|error| {
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

/// A caller the plugin sandbox confines that presented no credential at all.
pub fn unidentified_plugin_child() -> OrbitError {
    OrbitError::PolicyDenied(
        "a plugin backend descendant reached Orbit without the host-issued callback session; \
         changing process group or session does not make a confined child an ordinary caller, \
         and the credential must be carried through to every process that calls back"
            .to_string(),
    )
}

fn upsert_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env.push((key.to_string(), value));
    }
}
