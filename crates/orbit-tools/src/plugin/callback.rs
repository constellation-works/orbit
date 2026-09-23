//! Host-owned identity for a plugin backend that calls back into Orbit.
//!
//! The child controls its environment and its process tree: it can unset
//! every `ORBIT_*` variable the host stamped, and `setsid` gives it a pid,
//! ppid and pgid that match nothing the host recorded. Identity therefore
//! rides on something the child cannot rewrite and can only *drop*: the host
//! opens the per-call session record it wrote under
//! `{global_root}/state/plugin-callbacks/` and hands the backend that open
//! descriptor at [`PLUGIN_CALLBACK_FD`], close-on-exec cleared. Every
//! descendant inherits it across `fork` and `exec` — `setsid` does not close
//! descriptors — and a later `orbit tool run` or MCP `tools/call` reads the
//! record off the descriptor to learn which session it runs under.
//!
//! A descendant that closes the descriptor holds no credential and is
//! refused. That refusal is the sandbox's, not the process tree's: a confined
//! backend is denied the session directory (design
//! `docs/design/plugins/1_scope.md` §4.3), so a process that cannot even list
//! it is inside a plugin sandbox and a missing credential there is a refusal
//! rather than an ordinary caller.
//!
//! **A descriptor is a credential only if the host wrote what is on it.** A
//! backend can open any file it likes on the number, so three things must
//! agree: the descriptor is a regular file, it parses as a current-schema
//! record, and the record's own token names *that inode* inside the session
//! directory. The last check is what makes the credential unforgeable — a
//! plugin may write neither that directory nor any other plugin's record
//! (§4.3), so a file that answers to a name there is one the host wrote.
//!
//! The environment token plus pid/ppid/pgid ancestry that identified a
//! backend before this is kept for one release behind
//! `plugin.legacy_callback_identity`, which `orbit plugin doctor` reports for
//! as long as it is on. It is off by default: it is the path `setsid` escaped
//! [ORB-12798], and the descriptor does not have that shape.
//!
//! The record carries *authority* as well as identity: the effective tool
//! ceiling the spawning caller had when the host minted it. Knowing which
//! plugin is calling is not enough to decide a callback, because the same
//! plugin is reachable from callers with different allowlists — the
//! intersection is computed per call and the manifest list is not it
//! [ORB-12801].

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::process::ancestry::{
    current_parent_pid, current_process_group, process_start_key,
};
use orbit_types::plugin::PluginProvenance;
use serde::{Deserialize, Serialize};

use crate::upsert_env;

/// Informational namespace the backend already carries. Not the gate.
pub const ORBIT_PLUGIN_ENV: &str = "ORBIT_PLUGIN";

/// The descriptor the host maps the session record onto in a backend child.
///
/// Fixed, so a backend written in any language reads its credential the same
/// way and a descendant inherits it without being told. A shell backend must
/// therefore leave it alone: `exec 3<&-` throws the plugin's identity away
/// and every callback the child or its descendants make is refused.
pub const PLUGIN_CALLBACK_FD: i32 = 3;

/// Names the callback descriptor when it is not [`PLUGIN_CALLBACK_FD`].
///
/// Stamped by the host as information and as the seam a test uses; it is not
/// the gate. A child that rewrites or drops it only changes which number is
/// inspected, and a number holding anything but this host's own session
/// record is no credential at all.
pub const ORBIT_PLUGIN_CALLBACK_FD_ENV: &str = "ORBIT_PLUGIN_CALLBACK_FD";

/// Host-issued callback token, stamped into the backend child and inherited
/// by its descendants.
///
/// Retired: identity is [`PLUGIN_CALLBACK_FD`]. This variable is read only
/// while `plugin.legacy_callback_identity` is on, and is removed with the rest
/// of that path in the next release. A value that matches no session — or that
/// names a session belonging to another process — is a missing credential and
/// is refused. Dropping it inside the sandbox is a refusal too, never a way
/// out of the plugin's allowlist.
pub const ORBIT_PLUGIN_CALLBACK_ENV: &str = "ORBIT_PLUGIN_CALLBACK";

const SESSION_DIR: &str = "state/plugin-callbacks";
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Most a session record may occupy. Records are a few hundred bytes of JSON;
/// the cap bounds what an arbitrary descriptor on the callback number can make
/// this process read before it is rejected.
const MAX_SESSION_BYTES: usize = 8 * 1024;

/// Version 3 records carry their own token, which is what lets a child verify
/// that the descriptor it holds is the record the host wrote for it — the
/// token names the file, and only the host can put a file under that name.
/// Records carry the session's effective tool ceiling from version 2 on. A
/// record without one states no ceiling, so it is not parsed at all rather
/// than read as an unbounded session: these files live only as long as the
/// child they identify, and refusing a leftover from an older host is the
/// fail-closed half of the choice. The other half is the janitor's: a record
/// this host cannot read is counted stale by `orbit plugin doctor` and swept
/// when the next backend starts, so refusing it never leaves an orphan no
/// surface mentions [ORB-12879].
const SESSION_SCHEMA_VERSION: u32 = 3;

/// Live callback session the host minted for one backend child.
#[derive(Debug)]
pub struct PluginCallbackSession {
    path: PathBuf,
    token: String,
    record: SessionRecord,
    /// The host's own read handle on the record, opened at mint time and
    /// handed to the child as [`PLUGIN_CALLBACK_FD`]. Held for the session's
    /// life because that is the child's life: the `mcp` backend keeps one per
    /// live server, and the `exec` backend one per call.
    credential: File,
}

/// The plugin a resolved callback belongs to, and what that session may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCallbackIdentity {
    /// This session's plugin identity, exactly like the identity behind a
    /// call the plugin made directly — except `grants` is always empty: a
    /// callback's authority is `effective_tools`, minted from the spawning
    /// caller's ceiling, never the install row's recorded grants. A caller
    /// that also needs the row's grants (`stamp_callback_plugin_provenance`)
    /// merges them in itself.
    pub provenance: PluginProvenance,
    /// The tool ceiling the host minted this session with: the spawning
    /// caller's own `permissions.orbit_tools` ∩ grant ∩ `allowed_tools`
    /// intersection, sorted and deduped. A callback may never reach past it,
    /// whatever the plugin row says later [ORB-12801].
    pub effective_tools: Vec<String>,
}

impl PluginCallbackIdentity {
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
    /// The record's own file name under the session directory. A child holding
    /// the descriptor checks that this name resolves to the very inode it
    /// holds, which is what a forged record cannot arrange.
    token: String,
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
                token: token.clone(),
                pid: 0,
                starttime: 0,
            };
            match write_session_exclusive(&path, &record) {
                Ok(()) => {
                    let credential = open_credential(&path)?;
                    return Ok(Self {
                        path,
                        token,
                        record,
                        credential,
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

    /// Stamp the callback variables into the cleared-and-set child
    /// environment.
    ///
    /// Neither is the gate. `ORBIT_PLUGIN_CALLBACK_FD` tells a backend which
    /// descriptor carries its credential; `ORBIT_PLUGIN_CALLBACK` is the
    /// retired token, still stamped so an operator who turns
    /// `plugin.legacy_callback_identity` back on for a release does not have
    /// to respawn every live backend to make it work.
    pub fn stamp_env(&self, env: &mut Vec<(String, String)>) {
        upsert_env(env, ORBIT_PLUGIN_CALLBACK_ENV, self.token.clone());
        upsert_env(
            env,
            ORBIT_PLUGIN_CALLBACK_FD_ENV,
            PLUGIN_CALLBACK_FD.to_string(),
        );
    }

    /// The host descriptor the child must receive as [`PLUGIN_CALLBACK_FD`].
    ///
    /// Borrowed, not transferred: this session owns it and has to outlive the
    /// spawn that maps it.
    pub fn credential_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;

        self.credential.as_raw_fd()
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

/// Identify a plugin callback from the inherited credential descriptor, or —
/// while `legacy_identity` says so — from the host-issued token and process
/// ancestry. `None` is an ordinary caller.
///
/// Every other outcome is [`OrbitError::PolicyDenied`], never an absent
/// restriction: a token matching no session, a token belonging to another
/// process, a retired credential presented after the legacy path was turned
/// off, and a confined child carrying no credential at all. Ancestry still
/// names the plugin, where it can, so the refusal can be audited.
pub fn resolve_plugin_callback<F>(
    global_root: &Path,
    legacy_identity: F,
) -> Result<Option<PluginCallbackIdentity>, OrbitError>
where
    F: FnOnce() -> Result<bool, OrbitError>,
{
    match resolve_plugin_callback_session(global_root, legacy_identity)? {
        CallbackResolution::None => Ok(None),
        CallbackResolution::Identified(identity) => Ok(Some(identity)),
        CallbackResolution::InvalidCredential(identity) => Err(invalid_callback_credential(
            identity.as_ref().map(|id| id.provenance.name.as_str()),
        )),
        CallbackResolution::Mismatched { token, ancestry } => {
            Err(mismatched_callback_credential(&token, ancestry.as_deref()))
        }
        CallbackResolution::RetiredCredential(identity) => Err(retired_callback_credential(
            identity.as_ref().map(|id| id.provenance.name.as_str()),
        )),
        CallbackResolution::UnidentifiedPluginChild => Err(unidentified_plugin_child()),
    }
}

/// Full resolution used by dispatch so an invalid credential can still stamp
/// plugin identity on the audit row.
///
/// `legacy_identity` is the host's answer to "is
/// `plugin.legacy_callback_identity` still on". It is a callback rather than a
/// value because reading it costs a configuration load, and the question only
/// arises for a caller that presented legacy evidence — never on the ordinary
/// path every `orbit` invocation takes.
pub fn resolve_plugin_callback_session<F>(
    global_root: &Path,
    legacy_identity: F,
) -> Result<CallbackResolution, OrbitError>
where
    F: FnOnce() -> Result<bool, OrbitError>,
{
    let dir = callback_dir(global_root);
    // The credential this host issues. Checked first and on its own: it
    // survives `setsid` and a cleared environment, and it needs neither the
    // session directory nor the process tree to be readable.
    if let Some(record) = credential_from_descriptor(&dir) {
        return Ok(CallbackResolution::Identified(identity_from(&record)));
    }
    let token = std::env::var(ORBIT_PLUGIN_CALLBACK_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let caller = CallerProcess::current();
    let ancestry = scan_ancestry_session(global_root, &caller);
    // Nothing legacy to weigh: the answer is the same either way, so the flag
    // is never read. An unreadable session directory is still the sandbox
    // answering — that gate is not part of the deprecation [ORB-12798].
    if token.is_none() && ancestry.session().is_none() {
        return Ok(match ancestry {
            AncestryScan::Unreadable => CallbackResolution::UnidentifiedPluginChild,
            AncestryScan::None | AncestryScan::Session(_) => CallbackResolution::None,
        });
    }
    if !legacy_identity()? {
        return Ok(CallbackResolution::RetiredCredential(
            ancestry.session().map(identity_from),
        ));
    }
    tracing::warn!(
        target: "orbit.tools.plugin",
        "a plugin callback was identified by the deprecated environment token / process \
         ancestry path; it is removed in the next release. Clear \
         `plugin.legacy_callback_identity` and make sure the backend keeps file descriptor 3 \
         open across `setsid`, `exec` and any wrapper script.",
    );
    let token_record = match token.as_deref() {
        Some(value) => match load_token_session(&dir, value) {
            Ok(record) => TokenLookup::Found(record),
            Err(OrbitError::PolicyDenied(_)) => TokenLookup::Invalid,
            Err(error) => return Err(error),
        },
        None => TokenLookup::Absent,
    };
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
    /// The caller presented only the retired credential — the environment
    /// token, or an ancestry a live session still matches — while
    /// `plugin.legacy_callback_identity` is off. It held a credential this
    /// host no longer honours, which is a refusal and not an ordinary caller.
    /// `Some` when ancestry still names the backend so the refusal can be
    /// audited.
    RetiredCredential(Option<PluginCallbackIdentity>),
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
fn open_credential(path: &Path) -> Result<File, OrbitError> {
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

fn callback_dir(global_root: &Path) -> PathBuf {
    global_root.join(SESSION_DIR)
}

/// Resolve the existing callback directory under the selected host root.
/// Symlinked state subdirectories that escape that root are refused before a
/// directory scan can follow them.
fn validated_callback_session_dir(global_root: &Path) -> std::io::Result<PathBuf> {
    let root = global_root.canonicalize()?;
    let dir = callback_dir(&root).canonicalize()?;
    if !dir.starts_with(&root) || !std::fs::metadata(&dir)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "plugin callback directory is outside its host root or is not a directory",
        ));
    }
    Ok(dir)
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
    match orbit_common::fs::io::open_read_only_no_follow(&path) {
        Ok(file) => {
            let metadata = file.metadata().map_err(|error| {
                OrbitError::Io(format!(
                    "read plugin callback session `{}`: {error}",
                    path.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(invalid_callback_credential(None));
            }
            let mut bytes = Vec::new();
            file.take(MAX_SESSION_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    OrbitError::Io(format!(
                        "read plugin callback session `{}`: {error}",
                        path.display()
                    ))
                })?;
            if bytes.len() > MAX_SESSION_BYTES {
                return Err(invalid_callback_credential(None));
            }
            parse_session(&bytes).ok_or_else(|| invalid_callback_credential(None))
        }
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

fn scan_ancestry_session(global_root: &Path, caller: &CallerProcess) -> AncestryScan {
    // Landlock refuses `/proc/<pid>` of any other process (it would leak
    // `environ`). Identity therefore uses syscalls that still work in the
    // confined child: this pid, `getppid`, and `getpgrp`.
    //
    // A confined backend is not granted the session directory at all, so this
    // scan is how an unidentified plugin child is told apart from a local
    // caller: `EACCES` here is the sandbox answering, and an absent directory
    // means no session was ever minted on this host.
    let dir = match validated_callback_session_dir(global_root) {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return AncestryScan::Unreadable;
        }
        Err(_) => return AncestryScan::None,
    };
    let entries = match fs::read_dir(&dir) {
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

fn session_process_is_live(record: &SessionRecord) -> bool {
    process_start_key(record.pid).is_some_and(|key| key.starttime == record.starttime)
}

fn parse_session(bytes: &[u8]) -> Option<SessionRecord> {
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
///
/// Deliberately not `O_TRUNC`: by the time the host binds the child's pid the
/// child is already running and already holds this inode open as its
/// credential. Truncating first would leave a window in which the record it
/// reads is an empty file — a refused callback for a backend that did
/// everything right. `pid` and `starttime` only ever go from zero to real
/// values, so the rewrite covers the old bytes and the `set_len` after it is
/// the correctness backstop rather than the normal case.
fn rewrite_session_in_place(path: &Path, record: &SessionRecord) -> Result<(), OrbitError> {
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
