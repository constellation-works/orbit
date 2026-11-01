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

/// Backend marker, accepted only as evidence to refuse a caller without the
/// host-issued descriptor. Older hosts stamped their retired token here;
/// current hosts stamp a non-secret marker that never authorizes a callback.
pub const ORBIT_PLUGIN_CALLBACK_ENV: &str = "ORBIT_PLUGIN_CALLBACK";

pub(super) const SESSION_DIR: &str = "state/plugin-callbacks";
pub(super) const TOKEN_BYTES: usize = 32;
pub(super) const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Most a session record may occupy. Records are a few hundred bytes of JSON;
/// the cap bounds what an arbitrary descriptor on the callback number can make
/// this process read before it is rejected.
pub(super) const MAX_SESSION_BYTES: usize = 8 * 1024;

/// Version 5 marks an application invocation that may carry constrained
/// evidence (entry bindings, text digests, the predicate it was admitted on)
/// and a pin slot; a reader that only knows version 4 would ignore them.
/// Version 4 binds an optional application scope so an older reader cannot
/// ignore it and answer completion calls under a wider callback allowlist.
/// Version 3 records carried their own token, which is what lets a child verify
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
pub(super) const SESSION_SCHEMA_VERSION: u32 = 5;

/// The previous schema, still read so callbacks from backends a pre-upgrade
/// host started keep resolving, application-bound ones included: its
/// invocation reads with the newer fields defaulted, so it carries no pin and
/// matches exactly what it was admitted for.
pub(super) const PREVIOUS_SESSION_SCHEMA_VERSION: u32 = 4;

/// Live callback session the host minted for one backend child.
#[derive(Debug)]
pub struct PluginCallbackSession {
    path: PathBuf,
    record: SessionRecord,
    /// The host's own read handle on the record, opened at mint time and
    /// handed to the child as [`PLUGIN_CALLBACK_FD`]. Held for the session's
    /// life because that is the child's life: the `mcp` backend keeps one per
    /// live server, and the `exec` backend one per call.
    #[cfg_attr(not(unix), allow(dead_code))]
    credential: File,
    /// The route the child's decision-channel callbacks reach the host by,
    /// when its ceiling admits one ([`Self::open_host_route`]).
    #[cfg(unix)]
    host_route: Option<super::host_route::HostRoute>,
}

/// The plugin a resolved callback belongs to, and what that session may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCallbackIdentity {
    /// Application authority minted by the host for this child, if any.
    pub application: Option<Box<orbit_types::tool::ApplicationInvocation>>,
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
    #[serde(default)]
    pub(super) application: Option<Box<orbit_types::tool::ApplicationInvocation>>,
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
    /// Bind completion authority before spawning a child. The app credential
    /// is never delivered: this reference works only inside this host record.
    pub fn bind_application(
        &mut self,
        invocation: Option<&orbit_types::tool::ApplicationInvocation>,
    ) -> Result<(), OrbitError> {
        if invocation.is_none() && self.record.application.is_none() {
            return Ok(());
        }
        // A pin is host-route state, never part of what the child is handed.
        self.record.application = invocation.cloned().map(|mut invocation| {
            invocation.callback = true;
            invocation.pinned_task = None;
            Box::new(invocation)
        });
        if session_bytes(&self.record)?.len() > MAX_SESSION_BYTES {
            return Err(OrbitError::InvalidInput(
                "application callback scope exceeds the session byte limit".into(),
            ));
        }
        rewrite_session_in_place(&self.path, &self.record)
    }

    /// Create the session file and return a guard that unlinks it on drop.
    ///
    /// `effective_tools` is the spawning caller's intersection — what this
    /// one child is allowed to call back for, for as long as it lives.
    pub fn mint(
        global_root: &Path,
        provenance: &PluginProvenance,
        effective_tools: &[String],
    ) -> Result<Self, OrbitError> {
        Self::mint_with(global_root, provenance, effective_tools, open_credential)
    }

    /// [`Self::mint`] with the credential opener supplied. Opening fails only
    /// under descriptor exhaustion or on a host with no inherited descriptors,
    /// so a test drives that path through here.
    pub(crate) fn mint_with(
        global_root: &Path,
        provenance: &PluginProvenance,
        effective_tools: &[String],
        open: impl Fn(&Path) -> Result<File, OrbitError>,
    ) -> Result<Self, OrbitError> {
        let effective_tools = normalize_tools(effective_tools);
        let dir = callback_dir(global_root);
        ensure_private_session_dir(&dir).map_err(|error| {
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
                application: None,
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
                    // The record exists and nothing owns it yet: its `pid` is
                    // zero, so the sweep reads it as a mint in progress and
                    // never reaps it. Remove it rather than strand it.
                    let credential = match open(&path) {
                        Ok(credential) => credential,
                        Err(error) => {
                            let _ = fs::remove_file(&path);
                            return Err(error);
                        }
                    };
                    return Ok(Self {
                        path,
                        record,
                        credential,
                        #[cfg(unix)]
                        host_route: None,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    // `create_new` may have made the file before the write
                    // failed; a partial record is skipped by the sweep forever.
                    let _ = fs::remove_file(&path);
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

    /// Name the inherited descriptor and mark the child for refusal if it
    /// drops that descriptor. The marker carries no authority and preserves
    /// refusal after a detached unsandboxed child loses its ancestry match.
    pub fn stamp_env(&self, env: &mut Vec<(String, String)>) {
        upsert_env(
            env,
            ORBIT_PLUGIN_CALLBACK_ENV,
            "descriptor-required".to_string(),
        );
        upsert_env(
            env,
            ORBIT_PLUGIN_CALLBACK_FD_ENV,
            PLUGIN_CALLBACK_FD.to_string(),
        );
    }

    /// The host descriptor the child must receive as [`PLUGIN_CALLBACK_FD`].
    ///
    /// Borrowed, not transferred: this session owns it and has to outlive the
    /// spawn that maps it. Unix-only: no session is minted elsewhere.
    #[cfg(unix)]
    pub fn credential_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;

        self.credential.as_raw_fd()
    }

    /// Give the child a route back to `host` for the callbacks only the host
    /// can answer (`super::host_route::routes_on_host`), when this session's
    /// ceiling admits one of them and the caller supplied a host. Calls on the
    /// route are answered under this session for as long as it lives.
    ///
    /// Opened before the profile is built, so the spawn maps the route at
    /// [`super::PLUGIN_HOST_ROUTE_FD`] beside the credential.
    pub fn open_host_route(
        &mut self,
        host: Option<&std::sync::Arc<dyn crate::OrbitToolHost>>,
    ) -> Result<(), OrbitError> {
        #[cfg(unix)]
        if let Some(host) = host
            && self.host_route.is_none()
            && self
                .record
                .effective_tools
                .iter()
                .any(|tool| super::host_route::routes_on_host(&self.record, tool))
        {
            self.host_route = Some(super::host_route::HostRoute::start(
                self.record.clone(),
                std::sync::Arc::clone(host),
            )?);
        }
        #[cfg(not(unix))]
        let _ = host;
        Ok(())
    }

    /// The host descriptor the child must receive as
    /// [`super::PLUGIN_HOST_ROUTE_FD`], when [`Self::open_host_route`] opened
    /// one. Borrowed like [`Self::credential_fd`].
    #[cfg(unix)]
    pub fn host_route_fd(&self) -> Option<i32> {
        self.host_route
            .as_ref()
            .map(super::host_route::HostRoute::child_fd)
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

    /// Record the spawned child's pid and start time for stale-session cleanup
    /// and refusal of descendants that drop the callback descriptor.
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

/// Create the session directory owner-only and make an existing one so.
///
/// The records under it are the credential a backend's descriptor must name
/// (see the module docs), so who can list or open them matters: a directory
/// left `0755` by an older release or a permissive umask lets other local
/// users read live session records. This is the host's own directory, so a
/// looser one is tightened rather than refused.
fn ensure_private_session_dir(dir: &Path) -> std::io::Result<()> {
    orbit_common::fs::io::create_private_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

impl Drop for PluginCallbackSession {
    fn drop(&mut self) {
        // Stop answering routed calls before the record they run under goes.
        #[cfg(unix)]
        self.host_route.take();
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
