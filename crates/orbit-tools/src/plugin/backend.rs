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

use crate::builtin::proc::spawn::enforce_program_allowlist;
use crate::{TIMEOUT_SLOW_MS, ToolContext};

/// Host ceiling on `spec.backend.timeout_ms`.
pub const PLUGIN_TIMEOUT_CEILING_MS: u64 = 300_000;

/// The per-plugin facts a backend needs to run one of its tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginBackendSpec {
    pub provenance: PluginProvenance,
    pub plugin_root: PathBuf,
    /// `ORBIT_PLUGIN_STATE`: the host's per-plugin state directory.
    pub state_dir: PathBuf,
    /// Orbit's own global root. A backend granted `orbit_tools` calls
    /// `orbit tool run`, which reads and writes the stores under this root
    /// and the workspace's `.orbit/`, so the sandbox opens them for that
    /// grant and for no other.
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
        if self.granted(PluginGrant::OrbitTools) {
            // The callback runs `orbit tool run` in the child: its own
            // governance, allowlist and audit decide what that call may do,
            // but it cannot run at all without Orbit's roots.
            write.push(self.global_root.clone());
            if let Some(workspace_root) = workspace_root {
                write.push(workspace_root.join(".orbit"));
            }
        }
        let network = if self.granted(PluginGrant::Network) {
            self.permissions.network
        } else {
            PluginNetworkPermission::None
        };
        Ok(PluginSandboxProfile {
            read,
            write,
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
        // `orbit tool run` identifies a callback from `ORBIT_PLUGIN` and
        // enforces the recorded install, not this value.
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
    /// Readable (and executable) roots: the plugin root plus granted reads.
    pub read: Vec<PathBuf>,
    /// Writable roots: granted writes only.
    pub write: Vec<PathBuf>,
    pub network: PluginNetworkPermission,
    /// `backend.sandbox: none` with the `unsandboxed` grant: no confinement.
    pub unsandboxed: bool,
}

impl Sandbox for PluginSandboxProfile {
    fn validate(&self, _req: &ExecRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    /// Confine the child with the platform's provider: Landlock on Linux,
    /// `sandbox-exec` on macOS. Anywhere else the only way to run is the
    /// `unsandboxed` grant; there is no unconfined fallback (§4.9).
    fn spawn(&self, req: &ExecRequest) -> Result<Child, OrbitError> {
        // A granted write root is materialised before the child exists: the
        // grant names it, and neither a kernel rule nor an unconfined
        // backend can create a directory the grant's parent never allowed.
        for root in &self.write {
            if !root.exists() {
                std::fs::create_dir_all(root).map_err(|error| {
                    OrbitError::Io(format!(
                        "create granted write directory `{}`: {error}",
                        root.display()
                    ))
                })?;
            }
        }
        if self.unsandboxed {
            return NoSandbox.spawn(req);
        }
        spawn_confined(self, req)
    }
}

#[cfg(target_os = "linux")]
fn spawn_confined(profile: &PluginSandboxProfile, req: &ExecRequest) -> Result<Child, OrbitError> {
    let boundary = orbit_exec::LandlockBoundary {
        read: profile.read.clone(),
        write: profile.write.clone(),
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
        append_macos_network_access, compile_macos_sandbox_profile, spawn_under_macos_sandbox,
    };
    use orbit_types::policy::ResolvedFsProfile;
    use std::process::Stdio;

    let rules = ResolvedFsProfile {
        name: "plugin".to_string(),
        read: profile
            .read
            .iter()
            .map(|path| format!("{}/**", path.display()))
            .collect(),
        modify: profile
            .write
            .iter()
            .map(|path| format!("{}/**", path.display()))
            .collect(),
    };
    // "plugin" is not a provider name, so the compiler keeps every default
    // credential deny (it fails closed on an unknown provider).
    let mut profile_text = compile_macos_sandbox_profile(&rules, "plugin")?;
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
