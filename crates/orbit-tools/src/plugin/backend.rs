//! What every tool of one plugin shares: the backend program, the granted
//! profile the sandbox enforces, and the child environment (design §4.2,
//! §4.3).
//!
//! The manifest's `permissions` are requests. By the time a [`PluginBackendSpec`]
//! exists the host has checked that every required grant is recorded
//! (`orbit-core`'s plugin host registers the tools inactive otherwise), so
//! the profile resolved here is exactly the granted one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_common::security::child_env::{allowlisted_child_env, allowlisted_child_env_from};
use orbit_exec::{ExecRequest, NoSandbox, Sandbox};
use orbit_types::plugin::{
    PLUGIN_HOST_API, PluginGrant, PluginManifestError, PluginNetworkPermission, PluginPermissions,
    PluginProvenance, PluginSandbox, PluginTemplateVars, render_template,
};
use orbit_types::policy::ResolvedFsProfile;

use super::callback::PluginCallbackSession;
use crate::builtin::proc::spawn::enforce_program_allowlist;
use crate::{TIMEOUT_SLOW_MS, ToolContext};

/// Host ceiling on `spec.backend.timeout_ms`.
pub const PLUGIN_TIMEOUT_CEILING_MS: u64 = 300_000;

/// Store directories under Orbit's global root that a confined `orbit tool
/// run` appends to. This is the same inventory the agent sandbox grants a
/// nested Orbit process (`orbit-core`'s `append_linux_runtime_write_roots`),
/// and it is deliberately *not* the root itself: `bin/orbit` is executed
/// unconfined by the scheduler and every worker, `plugins/` records what each
/// plugin is allowed to do, and `config.toml`, `mcp-callers.toml` and
/// `clock.toml` are host configuration. Those stay readable and unwritable
/// [ORB-12777].
const ORBIT_TOOLS_GLOBAL_WRITE_DIRS: &[&str] = &["state/logs", "state/audit", "tasks"];

/// Individual files under the global root the child opens for writing: the
/// audit/task store's WAL file set, and the executable-generation locks every
/// `orbit` process pins before it bootstraps.
///
/// Each is granted only when it already exists as a regular file. A Landlock
/// rule binds an inode, so a name that is not there yet cannot be granted —
/// and materialising one here would be a write no grant made. The host
/// process that spawns the backend has already opened the store and pinned
/// its own generation, so the file set is present for the call.
const ORBIT_TOOLS_GLOBAL_WRITE_FILES: &[&str] = &[
    "orbit.db",
    "orbit.db-wal",
    "orbit.db-shm",
    ".generation.lock",
    ".generation-admission.lock",
];

/// Host-owned trees beneath the global root that no plugin child may read,
/// whatever else it was granted.
///
/// `state/plugin-callbacks/` holds the live callback credentials: a plugin
/// that could read the directory could present another plugin's token, and
/// one that could list it could enumerate every backend running on the host.
/// A confined child is granted its *own* record as a single file instead
/// ([`PluginSandboxProfile::with_callback_session`]), which is what its
/// `orbit tool run` reads to identify itself. `plugins/.grants/` holds the
/// grant witnesses that decide what each plugin is authorized to do; those are
/// the host's answer, never a plugin's input, and the child is likewise
/// granted only its own ([`plugin_grant_witness_relative`]) so it can verify
/// its own row and read nothing about any other plugin [ORB-12798].
const PLUGIN_GLOBAL_READ_DENY_DIRS: &[&str] = &["state/plugin-callbacks", "plugins/.grants"];

/// Host-owned directory beside the namespace install directories, holding one
/// grant-authorization witness per plugin (`orbit-core`'s `plugin_grants`).
/// Named here because the sandbox decides who may read it.
pub const PLUGIN_GRANT_WITNESS_DIR: &str = ".grants";

/// Where `plugin`'s witness sits, relative to the global root.
pub fn plugin_grant_witness_relative(plugin: &str) -> PathBuf {
    Path::new("plugins")
        .join(PLUGIN_GRANT_WITNESS_DIR)
        .join(format!("{plugin}.json"))
}

/// The same narrowing for the workspace's `.orbit/`: the stores a callback
/// writes, never `plugins.yaml` (the install pin), `routines/`, `auto_tasks/`
/// or `config.toml`.
const ORBIT_TOOLS_WORKSPACE_WRITE_DIRS: &[&str] = &[
    "tasks",
    "frictions",
    "state/audit",
    "state/logs",
    "state/job-runs",
];

/// The workspace lexical index's WAL file set, granted on the same terms as
/// [`ORBIT_TOOLS_GLOBAL_WRITE_FILES`]. Callback session files live at
/// `state/plugin-callbacks/` under the global root, which is absent from
/// both write inventories: the host writes them, and the child must not.
const ORBIT_TOOLS_WORKSPACE_WRITE_FILES: &[&str] = &[
    "state/semantic.db",
    "state/semantic.db-wal",
    "state/semantic.db-shm",
];

/// The per-plugin facts a backend needs to run one of its tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginBackendSpec {
    pub provenance: PluginProvenance,
    pub plugin_root: PathBuf,
    /// `ORBIT_PLUGIN_STATE`: the host's per-plugin state directory.
    pub state_dir: PathBuf,
    /// Orbit's own global root. A backend granted `orbit_tools` calls
    /// `orbit tool run`, which reads this root and the workspace's `.orbit/`
    /// and appends to some of the stores beneath them, so the sandbox opens
    /// them for that grant and for no other — the roots read-only, the stores
    /// by name (see [`ORBIT_TOOLS_GLOBAL_WRITE_DIRS`]).
    pub global_root: PathBuf,
    /// The resolved backend program and its fixed arguments.
    pub command: PathBuf,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub sandbox: PluginSandbox,
    /// The manifest's requests; every one of them the host has granted.
    pub permissions: PluginPermissions,
    /// `spec.requires.programs`: what the backend declares it spawns.
    pub programs: Vec<String>,
    /// `spec.config.defaults`, the only source of `{{config.<key>}}` until
    /// `[plugins.<ns>]` admission lands (phase 3).
    pub config_defaults: BTreeMap<String, String>,
    /// The grants recorded at enable time.
    pub grants: Vec<PluginGrant>,
}

impl PluginBackendSpec {
    pub fn granted(&self, grant: PluginGrant) -> bool {
        self.grants.contains(&grant)
    }

    /// The effective timeout for one call, capped by the host ceiling.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .unwrap_or(TIMEOUT_SLOW_MS)
            .min(PLUGIN_TIMEOUT_CEILING_MS)
    }

    fn template_vars(&self, workspace_root: Option<&Path>) -> PluginTemplateVars {
        PluginTemplateVars {
            workspace: workspace_root.map(|path| path.to_string_lossy().into_owned()),
            plugin_root: self.plugin_root.to_string_lossy().into_owned(),
            plugin_state: self.state_dir.to_string_lossy().into_owned(),
            config: self.config_defaults.clone(),
        }
    }

    /// The filesystem and network boundary for a call made from
    /// `workspace_root`: the manifest's fs paths rendered, plus the plugin
    /// root itself, which holds the backend the child must execute.
    pub fn sandbox_profile(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<PluginSandboxProfile, OrbitError> {
        let vars = self.template_vars(workspace_root);
        let render = |paths: &[String], key: &str| -> Result<Vec<PathBuf>, OrbitError> {
            paths
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    render_template(path, &vars, &format!("spec.permissions.fs.{key}[{index}]"))
                        .map(PathBuf::from)
                        .map_err(plugin_refusal)
                })
                .collect()
        };
        let mut read = vec![self.plugin_root.clone()];
        if self.granted(PluginGrant::Fs) {
            read.extend(render(&self.permissions.fs.read, "read")?);
        }
        let mut write = if self.granted(PluginGrant::Fs) {
            render(&self.permissions.fs.write, "write")?
        } else {
            Vec::new()
        };
        for (index, path) in write.iter().enumerate() {
            if let Some(protected) = super::loader::fs_write_root_covers(
                path,
                &self.plugin_root,
                &self.global_root,
                &self.state_dir,
                workspace_root,
            ) {
                return Err(plugin_refusal(PluginManifestError::new(
                    format!("spec.permissions.fs.write[{index}]"),
                    format!(
                        "'{}' grants write access to the {protected}; a plugin cannot request \
                         writes to its own install tree or anywhere beneath Orbit's global root \
                         except its own plugin state tree, or to workspace metadata `.orbit` / `.git`",
                        self.permissions
                            .fs
                            .write
                            .get(index)
                            .map(String::as_str)
                            .unwrap_or("")
                    ),
                )));
            }
        }
        let mut write_files = Vec::new();
        if self.granted(PluginGrant::OrbitTools) {
            // The callback runs `orbit tool run` in the child: its own
            // governance, allowlist and audit decide what that call may do,
            // but it cannot run at all without reading Orbit's roots and
            // appending to Orbit's stores. Those are two different sets. The
            // roots are granted read-only and the stores are named one by
            // one, so holding `orbit_tools` no longer carries the right to
            // rewrite the binary the scheduler runs unconfined, the recorded
            // plugin installs, or the MCP authorization ceiling [ORB-12777].
            read.push(self.global_root.clone());
            // Its own witness, and no other plugin's: the nested `orbit tool
            // run` verifies the grants recorded for this plugin before it
            // registers the row, and the witness directory itself is denied
            // above. A confined child therefore cannot read what any other
            // plugin was authorized for [ORB-12798].
            read.push(
                self.global_root
                    .join(plugin_grant_witness_relative(&self.provenance.name)),
            );
            for relative in ORBIT_TOOLS_GLOBAL_WRITE_DIRS {
                write.push(self.global_root.join(relative));
            }
            for relative in ORBIT_TOOLS_GLOBAL_WRITE_FILES {
                write_files.push(self.global_root.join(relative));
            }
            if let Some(workspace_root) = workspace_root {
                let workspace_orbit = workspace_root.join(".orbit");
                for relative in ORBIT_TOOLS_WORKSPACE_WRITE_DIRS {
                    write.push(workspace_orbit.join(relative));
                }
                for relative in ORBIT_TOOLS_WORKSPACE_WRITE_FILES {
                    write_files.push(workspace_orbit.join(relative));
                }
                read.push(workspace_orbit);
            }
        }
        let network = if self.granted(PluginGrant::Network) {
            self.permissions.network
        } else {
            PluginNetworkPermission::None
        };
        Ok(PluginSandboxProfile {
            read,
            read_denies: PLUGIN_GLOBAL_READ_DENY_DIRS
                .iter()
                .map(|relative| self.global_root.join(relative))
                .collect(),
            write,
            write_files,
            materialization_roots: workspace_root
                .into_iter()
                .map(Path::to_path_buf)
                .chain(std::iter::once(self.state_dir.clone()))
                .collect(),
            network,
            unsandboxed: self.sandbox == PluginSandbox::None
                && self.granted(PluginGrant::Unsandboxed),
        })
    }

    /// The child environment for one call: the allowlisted baseline, the
    /// granted `env_pass` names copied from this process, and the Orbit
    /// plugin variables (design §4.2). `tool_name` is absent for a
    /// long-lived `mcp` child, which serves every tool.
    ///
    /// `env_pass` is composed through the same admission path as the
    /// baseline (`allowlisted_child_env_from`), not a raw name lookup: a
    /// manifest names a variable to request it, but the excluded
    /// privilege-bearing `ORBIT_*` names (`ORBIT_OPERATOR`,
    /// `ORBIT_WORKSPACE_CLAIM_TOKEN`) can never ride that request through
    /// even if present in the parent environment (`validate_structure`
    /// refuses them at the manifest too; this is the defense-in-depth
    /// boundary for a manifest loaded before that check existed).
    pub fn child_environment(
        &self,
        ctx: &ToolContext,
        cwd: &str,
        tool_name: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut env_pairs = ctx
            .proc_spawn_environment
            .clone()
            .unwrap_or_else(|| allowlisted_child_env(&[], &[]));
        if self.granted(PluginGrant::EnvPass) {
            let parent = ctx
                .proc_spawn_environment
                .clone()
                .unwrap_or_else(|| std::env::vars().collect());
            let admitted = allowlisted_child_env_from(&parent, &self.permissions.env_pass, &[]);
            for name in &self.permissions.env_pass {
                if let Some((_, value)) = admitted
                    .iter()
                    .find(|(admitted_name, _)| admitted_name == name)
                {
                    upsert_env(&mut env_pairs, name, value.clone());
                }
            }
        }
        let mut set = |key: &str, value: String| upsert_env(&mut env_pairs, key, value);
        set("ORBIT_HOST_API", PLUGIN_HOST_API.to_string());
        set("ORBIT_VERSION", env!("CARGO_PKG_VERSION").to_string());
        set("ORBIT_PLUGIN", self.provenance.name.clone());
        set("ORBIT_PLUGIN_VERSION", self.provenance.version.clone());
        set(
            "ORBIT_PLUGIN_ROOT",
            self.plugin_root.to_string_lossy().into_owned(),
        );
        set(
            "ORBIT_PLUGIN_STATE",
            self.state_dir.to_string_lossy().into_owned(),
        );
        if let Some(tool_name) = tool_name {
            set("ORBIT_TOOL_NAME", tool_name.to_string());
        }
        set("ORBIT_TOOL_CWD", cwd.to_string());
        if let Some(workspace_root) = ctx.workspace_root.as_ref() {
            set(
                "ORBIT_WORKSPACE_ROOT",
                workspace_root.to_string_lossy().into_owned(),
            );
        }
        // Always present, even when empty: information for the backend.
        // The callback gate does not read this value; identity is the
        // host-issued session (`ORBIT_PLUGIN_CALLBACK` plus ancestry).
        set("ORBIT_ALLOWED_TOOLS", self.allowed_tools(ctx).join(","));
        // `requires.programs` is what a callback through `proc.spawn` may run.
        set("ORBIT_PROC_ALLOWED_PROGRAMS", self.programs.join(","));
        env_pairs
    }

    /// `permissions.orbit_tools` ∩ the `orbit_tools` grant ∩ the caller's own
    /// allowlist when it has one (§4.2).
    pub fn allowed_tools(&self, ctx: &ToolContext) -> Vec<String> {
        if !self.granted(PluginGrant::OrbitTools) {
            return Vec::new();
        }
        self.permissions
            .orbit_tools
            .iter()
            .filter(|tool| {
                ctx.allowed_tools.is_empty()
                    || ctx.allowed_tools.iter().any(|allowed| allowed == *tool)
            })
            .cloned()
            .collect()
    }

    /// A caller whose own context restricts programs (an activity-scoped
    /// run) bounds what the plugin may spawn: every program the manifest
    /// declares must be on the caller's list, checked through the same gate
    /// `proc.spawn` uses.
    pub fn enforce_programs(&self, ctx: &ToolContext, tool_name: &str) -> Result<(), OrbitError> {
        for program in &self.programs {
            enforce_program_allowlist(ctx, tool_name, program)?;
        }
        Ok(())
    }
}

fn upsert_env(env_pairs: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(existing) = env_pairs.iter_mut().find(|(name, _)| name == key) {
        existing.1 = value;
    } else {
        env_pairs.push((key.to_string(), value));
    }
}

/// A manifest problem discovered at call time is still the manifest's fault.
fn plugin_refusal(error: PluginManifestError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

/// The granted boundary one plugin backend runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSandboxProfile {
    /// Readable (and executable) roots: the plugin root plus granted reads,
    /// and — for `orbit_tools` — Orbit's global root and the workspace's
    /// `.orbit/`, which `orbit tool run` reads but must not rewrite.
    pub read: Vec<PathBuf>,
    /// Host-owned trees carved out of [`Self::read`] however it was composed:
    /// the live callback sessions and the grant witnesses
    /// ([`PLUGIN_GLOBAL_READ_DENY_DIRS`]). Neither platform lets a manifest
    /// buy them back, because the carve-out is applied after the granted
    /// paths rather than beside them.
    pub read_denies: Vec<PathBuf>,
    /// Writable directories: granted writes only. Materialised before the
    /// child starts, because a rule cannot bind an inode that is not there.
    pub write: Vec<PathBuf>,
    /// Writable single files, granted only where one already exists as a
    /// regular file. Kept apart from [`Self::write`] so a named store file
    /// is never created as a directory, and so neither platform widens a
    /// leaf grant into its parent tree.
    pub write_files: Vec<PathBuf>,
    /// Roots beneath which the host may materialize an absent granted write
    /// directory. Other grants can name existing host paths, but creating
    /// those paths is never part of spawning a plugin.
    pub(crate) materialization_roots: Vec<PathBuf>,
    pub network: PluginNetworkPermission,
    /// `backend.sandbox: none` with the `unsandboxed` grant: no confinement.
    pub unsandboxed: bool,
}

impl PluginSandboxProfile {
    /// Grant the child read access to the one callback record that identifies
    /// it, inside the otherwise denied session directory.
    ///
    /// This is the credential `orbit tool run` reads back in the child. The
    /// grant is a single file: the directory stays unlistable and every other
    /// plugin's live token stays unreadable.
    #[must_use]
    pub fn with_callback_session(mut self, session: &PluginCallbackSession) -> Self {
        self.read.push(session.path().to_path_buf());
        self
    }

    /// The callback records this profile re-allows inside a denied directory:
    /// what [`Self::with_callback_session`] granted, and nothing else.
    pub fn readable_denied_files(&self) -> Vec<PathBuf> {
        self.read
            .iter()
            .filter(|path| {
                self.read_denies
                    .iter()
                    .any(|denied| path.starts_with(denied))
            })
            .cloned()
            .collect()
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
        // A granted write directory inside the workspace or plugin state is
        // materialised before the child exists: the grant names it, and
        // neither a kernel rule nor an unconfined backend can create a
        // directory the grant's parent never allowed. Host paths outside
        // those roots are never created here, and every component we do
        // create is checked without following symbolic links.
        // `write_files` is deliberately absent here — those name store files
        // SQLite and the generation protocol own, and creating one as an
        // empty directory would break the store rather than confine it.
        for root in &self.write {
            materialize_write_directory(root, &self.materialization_roots)?;
        }
        if self.unsandboxed {
            return NoSandbox.spawn(req);
        }
        spawn_confined(self, req)
    }
}

/// Create an absent write root only when its normalized path is contained by
/// a host-owned materialization root. Existing prefixes are inspected with
/// `symlink_metadata`, so directory creation never walks through a link into
/// an unrelated host tree.
fn materialize_write_directory(root: &Path, allowed_roots: &[PathBuf]) -> Result<(), OrbitError> {
    let root = super::loader::lexical_normalize(root);
    let Some(allowed) = allowed_roots
        .iter()
        .map(|allowed| super::loader::lexical_normalize(allowed))
        .find(|allowed| root == *allowed || root.starts_with(allowed))
    else {
        return Ok(());
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
fn spawn_confined(profile: &PluginSandboxProfile, req: &ExecRequest) -> Result<Child, OrbitError> {
    let boundary = orbit_exec::LandlockBoundary {
        read: profile.read.clone(),
        read_denies: profile.read_denies.clone(),
        write: profile.write.clone(),
        write_files: profile.write_files.clone(),
        // Landlock has no address filter: `loopback` and `any` both leave
        // TCP open, and only `none` is held at the kernel.
        deny_tcp: profile.network == PluginNetworkPermission::None,
    };
    orbit_exec::spawn_under_linux_landlock_boundary(req, &boundary)
}

#[cfg(target_os = "macos")]
fn spawn_confined(profile: &PluginSandboxProfile, req: &ExecRequest) -> Result<Child, OrbitError> {
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
    // denials appended after it; the child's own callback record is re-allowed
    // last. SBPL is last-match-wins.
    append_macos_read_boundary(
        &mut profile_text,
        &profile.read_denies,
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
    })?;
    Ok(child)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn spawn_confined(_profile: &PluginSandboxProfile, req: &ExecRequest) -> Result<Child, OrbitError> {
    Err(OrbitError::PolicyDenied(format!(
        "plugin backend `{}` cannot run confined on {}: Orbit sandboxes plugins with Landlock \
         (Linux) or sandbox-exec (macOS); a manifest may opt out with `backend.sandbox: none` \
         and the `unsandboxed` grant",
        req.program,
        std::env::consts::OS
    )))
}
