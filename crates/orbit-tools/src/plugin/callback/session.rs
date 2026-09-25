use super::*;

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

pub(super) const SESSION_DIR: &str = "state/plugin-callbacks";
pub(super) const TOKEN_BYTES: usize = 32;
pub(super) const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Most a session record may occupy. Records are a few hundred bytes of JSON;
/// the cap bounds what an arbitrary descriptor on the callback number can make
/// this process read before it is rejected.
pub(super) const MAX_SESSION_BYTES: usize = 8 * 1024;

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
pub(super) const SESSION_SCHEMA_VERSION: u32 = 3;

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
pub(super) struct SessionRecord {
    pub(super) schema_version: u32,
    pub(super) plugin: String,
    pub(super) version: String,
    pub(super) manifest_digest: String,
    /// Host-owned authority, fixed at mint time. The child can rewrite
    /// `ORBIT_ALLOWED_TOOLS` and `ORBIT_ACTIVITY_TOOLS` in its own
    /// environment; it cannot write this directory at all (§4.3).
    pub(super) effective_tools: Vec<String>,
    /// The record's own file name under the session directory. A child holding
    /// the descriptor checks that this name resolves to the very inode it
    /// holds, which is what a forged record cannot arrange.
    pub(super) token: String,
    pub(super) pid: u32,
    pub(super) starttime: u64,
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
