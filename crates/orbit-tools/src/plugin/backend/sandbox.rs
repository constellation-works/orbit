use super::*;

/// The granted boundary one plugin backend runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSandboxProfile {
    /// Readable (and executable) roots: the plugin root plus granted reads,
    /// and — for `orbit_tools` — Orbit's global root and the workspace's
    /// `.orbit/`, which `orbit tool run` reads but must not rewrite.
    pub read: Vec<PathBuf>,
    /// Host-owned trees carved out of [`Self::read`] however it was composed:
    /// the live callback sessions, the grant witnesses and every plugin's
    /// state ([`PLUGIN_GLOBAL_READ_DENY_DIRS`]). Neither platform lets a
    /// manifest buy them back: the carve-out is applied after the granted
    /// paths rather than beside them, and a manifest read root inside one of
    /// them never reaches [`Self::read`].
    pub read_denies: Vec<PathBuf>,
    /// This plugin's own `{{plugin_state}}`, which [`Self::read`] always
    /// carries: the one tree inside [`Self::read_denies`] re-allowed whole
    /// rather than as a single file.
    pub state_dir: PathBuf,
    /// Writable directories: granted writes only. Materialised before the
    /// child starts when they are beneath a host-owned materialization root;
    /// granted host paths outside those roots must already exist because a
    /// rule cannot bind an inode that is not there.
    pub write: Vec<PathBuf>,
    /// Writable single files, granted only where one already exists as a
    /// regular file. Kept apart from [`Self::write`] so a named store file
    /// is never created as a directory, and so neither platform widens a
    /// leaf grant into its parent tree.
    pub write_files: Vec<PathBuf>,
    /// Roots beneath which the host may materialize an absent granted write
    /// directory: the selected workspace, this plugin's state tree, and every
    /// directory the host itself added to [`Self::write`] for the
    /// `orbit_tools` grant. Other grants can name existing host paths, but
    /// creating those paths is never part of spawning a plugin.
    ///
    /// Derived from the same inventories that produce the host-added writes,
    /// so the two lists cannot drift apart [ORB-12872].
    pub(crate) materialization_roots: Vec<PathBuf>,
    pub network: PluginNetworkPermission,
    /// `backend.sandbox: none` with the `unsandboxed` grant: no confinement.
    pub unsandboxed: bool,
    /// The host descriptor the child receives as [`PLUGIN_CALLBACK_FD`]: its
    /// callback credential. Borrowed from the [`PluginCallbackSession`], which
    /// owns it and must outlive the spawn.
    pub(crate) callback_fd: Option<i32>,
}

impl PluginSandboxProfile {
    /// Carry one live callback session into the spawn.
    ///
    /// Two things travel together. The child receives the open record as
    /// [`PLUGIN_CALLBACK_FD`] — the credential itself, which survives a
    /// cleared environment and a `setsid`. And it is granted read access to
    /// that one record *by name*, inside the otherwise denied session
    /// directory, so it can confirm the descriptor it holds is the record the
    /// host wrote for it. The grant is a single file: the directory stays
    /// unlistable and every other plugin's live record stays out of reach.
    #[must_use]
    pub fn with_callback_session(mut self, session: &PluginCallbackSession) -> Self {
        self.read.push(session.path().to_path_buf());
        self.callback_fd = Some(session.credential_fd());
        self
    }

    /// The single files this profile re-allows inside a denied directory:
    /// the callback record [`Self::with_callback_session`] granted and the
    /// plugin's own grant witness, and nothing else.
    pub fn readable_denied_files(&self) -> Vec<PathBuf> {
        self.read
            .iter()
            .filter(|path| self.is_denied(path) && !path.starts_with(&self.state_dir))
            .cloned()
            .collect()
    }

    /// The trees this profile re-allows inside a denied directory: the
    /// plugin's own state, when it sits beneath a denied tree.
    pub fn readable_denied_trees(&self) -> Vec<PathBuf> {
        if self.is_denied(&self.state_dir) {
            vec![self.state_dir.clone()]
        } else {
            Vec::new()
        }
    }

    fn is_denied(&self, path: &Path) -> bool {
        self.read_denies
            .iter()
            .any(|denied| path.starts_with(denied))
    }

    /// The seatbelt view of this boundary.
    ///
    /// Not gated on the host OS: the macOS spawn path compiles it, and a
    /// Linux test compiles it to check the two platforms express the same
    /// write set. Directories become `subpath` roots (`<dir>/**`); a named
    /// file is emitted literally, so `orbit.db` never widens into the global
    /// root that holds it.
    pub fn macos_fs_rules(&self) -> ResolvedFsProfile {
        ResolvedFsProfile {
            name: "plugin".to_string(),
            read: self
                .read
                .iter()
                .map(|path| format!("{}/**", path.display()))
                .collect(),
            modify: self
                .write
                .iter()
                .map(|path| format!("{}/**", path.display()))
                .chain(
                    self.write_files
                        .iter()
                        .map(|path| path.display().to_string()),
                )
                .collect(),
        }
    }
}

impl Sandbox for PluginSandboxProfile {
    fn validate(&self, _req: &ExecRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    /// Confine the child with the platform's provider: Landlock on Linux,
    /// `sandbox-exec` on macOS. Anywhere else the only way to run is the
    /// `unsandboxed` grant; there is no unconfined fallback (§4.9).
    fn spawn(&self, req: &ExecRequest) -> Result<Child, OrbitError> {
        // A granted write directory beneath a host-materialization root — the
        // selected workspace, this plugin's state tree, or one of the Orbit
        // stores the host itself granted for `orbit_tools` — is materialised
        // before the child exists: the grant names it, and neither a kernel
        // rule nor an unconfined backend can create a directory the grant's
        // parent never allowed. Host paths outside those roots are never
        // created here and must already exist; every component we do create
        // is checked without following symbolic links.
        // `write_files` is deliberately absent here — those name store files
        // SQLite and the generation protocol own, and creating one as an
        // empty directory would break the store rather than confine it.
        for root in &self.write {
            materialize_write_directory(root, &self.materialization_roots)?;
        }
        // The callback credential reaches the child the same way under every
        // confinement: `sandbox-exec` execs the program it wraps and Landlock
        // governs paths, not descriptors, so an inherited number survives both.
        let inherited: Vec<InheritedFd> = self
            .callback_fd
            .map(|source| InheritedFd {
                source,
                target: PLUGIN_CALLBACK_FD,
            })
            .into_iter()
            .collect();
        if self.unsandboxed {
            return orbit_exec::spawn_with_inherited_fds(req, &inherited);
        }
        spawn_confined(self, req, &inherited)
    }
}

/// Create an absent write root only when its normalized path is contained by
/// a host-owned materialization root. Other granted roots must already exist.
/// Existing prefixes are inspected with `symlink_metadata`, so directory
/// creation never walks through a link into an unrelated host tree.
fn materialize_write_directory(root: &Path, allowed_roots: &[PathBuf]) -> Result<(), OrbitError> {
    let root = orbit_exec::lexical_normalize(root);
    let Some(allowed) = allowed_roots
        .iter()
        .map(|allowed| orbit_exec::lexical_normalize(allowed))
        .find(|allowed| root == *allowed || root.starts_with(allowed))
    else {
        return match std::fs::metadata(&root) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => Err(OrbitError::InvalidInput(format!(
                "granted write directory `{}` is not a directory",
                root.display()
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` does not exist; Orbit creates absent plugin \
                     write directories only inside the selected workspace, the plugin state \
                     directory, or the Orbit stores the `orbit_tools` grant opens; create this \
                     consented directory before running the plugin",
                    root.display()
                )))
            }
            Err(error) => Err(OrbitError::Io(format!(
                "inspect granted write directory `{}`: {error}",
                root.display()
            ))),
        };
    };

    // Start at the closest existing ancestor of the trusted root. This lets
    // platform aliases before that root (for example macOS `/var` ->
    // `/private/var`) resolve once, while every component controlled beneath
    // the workspace/plugin-state boundary remains subject to the no-link
    // walk below.
    let mut anchor = allowed.clone();
    loop {
        match std::fs::symlink_metadata(&anchor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(OrbitError::InvalidInput(format!(
                    "plugin write materialization root `{}` must not be a symbolic link",
                    anchor.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => {
                return Err(OrbitError::InvalidInput(format!(
                    "plugin write materialization root `{}` is not a directory",
                    anchor.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && anchor.pop() => {}
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect plugin write materialization root `{}`: {error}",
                    anchor.display()
                )));
            }
        }
    }
    let canonical_anchor = anchor.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize plugin write materialization root `{}`: {error}",
            anchor.display()
        ))
    })?;
    let relative = root.strip_prefix(&anchor).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "granted write directory `{}` is not below materialization anchor `{}`: {error}",
            root.display(),
            anchor.display()
        ))
    })?;
    let root = canonical_anchor.join(relative);

    let mut current = canonical_anchor;
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` resolves through symbolic link `{}`; plugin \
                     write directories may not follow symbolic links",
                    root.display(),
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` resolves through non-directory `{}`",
                    root.display(),
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&current) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
                            OrbitError::Io(format!(
                                "inspect concurrently created write directory `{}`: {error}",
                                current.display()
                            ))
                        })?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(OrbitError::InvalidInput(format!(
                                "granted write directory `{}` acquired an unsafe component `{}`",
                                root.display(),
                                current.display()
                            )));
                        }
                    }
                    Err(error) => {
                        return Err(OrbitError::Io(format!(
                            "create granted write directory `{}`: {error}",
                            current.display()
                        )));
                    }
                }
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect granted write directory `{}`: {error}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn spawn_confined(
    profile: &PluginSandboxProfile,
    req: &ExecRequest,
    inherited_fds: &[InheritedFd],
) -> Result<Child, OrbitError> {
    let boundary = orbit_exec::LandlockBoundary {
        read: profile.read.clone(),
        read_denies: profile.read_denies.clone(),
        write: profile.write.clone(),
        write_files: profile.write_files.clone(),
        // Landlock has no address filter: `loopback` and `any` both leave
        // TCP open, and only `none` is held at the kernel.
        deny_tcp: profile.network == PluginNetworkPermission::None,
    };
    orbit_exec::spawn_under_linux_landlock_boundary(req, &boundary, inherited_fds)
}

#[cfg(target_os = "macos")]
fn spawn_confined(
    profile: &PluginSandboxProfile,
    req: &ExecRequest,
    inherited_fds: &[InheritedFd],
) -> Result<Child, OrbitError> {
    use orbit_exec::{
        EnvironmentMode, MacosNetworkAccess, MacosSandboxSpawnRequest, StdinMode,
        append_macos_network_access, append_macos_read_boundary, compile_macos_sandbox_profile,
        spawn_under_macos_sandbox,
    };
    use std::process::Stdio;

    let rules = profile.macos_fs_rules();
    // "plugin" is not a provider name, so the compiler keeps every default
    // credential deny (it fails closed on an unknown provider).
    let mut profile_text = compile_macos_sandbox_profile(&rules, "plugin")?;
    // The compiler allows reads broadly, so the plugin's read carve-outs are
    // denials appended after it; the child's own state tree, callback record
    // and witness are re-allowed last. SBPL is last-match-wins.
    append_macos_read_boundary(
        &mut profile_text,
        &profile.read_denies,
        &profile.readable_denied_trees(),
        &profile.readable_denied_files(),
    );
    append_macos_network_access(
        &mut profile_text,
        match profile.network {
            PluginNetworkPermission::None => MacosNetworkAccess::None,
            PluginNetworkPermission::Loopback => MacosNetworkAccess::Loopback,
            PluginNetworkPermission::Any => MacosNetworkAccess::Any,
        },
    );
    let env = match &req.environment_mode {
        EnvironmentMode::ClearAndSet(pairs) => pairs.clone(),
        EnvironmentMode::Inherit => std::env::vars().collect(),
    };
    let stdin = match req.stdin_mode {
        StdinMode::Inherit => Stdio::inherit(),
        StdinMode::Null => Stdio::null(),
        StdinMode::Bytes(_) => Stdio::piped(),
    };
    let (child, _profile_file) = spawn_under_macos_sandbox(MacosSandboxSpawnRequest {
        profile_text: &profile_text,
        program: &req.program,
        args: &req.args,
        env: &env,
        cwd: req.current_dir.as_deref().map(Path::new),
        stdin,
        stdout: Stdio::piped(),
        stderr: Stdio::piped(),
        inherited_fds,
    })?;
    Ok(child)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_confined(
    _profile: &PluginSandboxProfile,
    req: &ExecRequest,
    _inherited_fds: &[InheritedFd],
) -> Result<Child, OrbitError> {
    Err(OrbitError::PolicyDenied(format!(
        "plugin backend `{}` cannot run confined on {}: Orbit sandboxes plugins with Landlock \
         (Linux) or sandbox-exec (macOS); a manifest may opt out with `backend.sandbox: none` \
         and the `unsandboxed` grant",
        req.program,
        std::env::consts::OS
    )))
}
