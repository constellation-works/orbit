//! The installation, platform, and workspace an update runs against.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use orbit_common::security::release::TrustedReleaseKey;
use orbit_common::{Env, OrbitError};

use super::source::release_source_from_env;
use crate::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::app::update::channel::{self, InstallChannel};
use orbit_core::app::update::converge;
use orbit_core::app::update::{UpdateEnvironment, UpdateWorkspace, admission_authorities};

/// Resolve the initialized workspace this process's convergence steps use.
///
/// An explicit `--root` or `ORBIT_ROOT` must already be an initialized
/// workspace. `--preflight` uses [`workspace_for_preflight`] so it
/// can probe an uninitialized generation root without this requirement.
pub fn workspace_for_process(
    root_override: Option<&Path>,
    env: &Env,
) -> Result<Option<UpdateWorkspace>, OrbitError> {
    let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
    let root_was_explicit = root_override.is_some()
        || env
            .orbit_root()
            .and_then(OsStr::to_str)
            .is_some_and(|root| !root.trim().is_empty());
    let roots = RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?;
    Ok(roots.map(|roots| {
        let root_argument = root_was_explicit.then(|| roots.shared_root.clone());
        UpdateWorkspace {
            cwd,
            root: roots.shared_root,
            root_argument,
        }
    }))
}

/// Workspace whose convergence `--preflight` should admit, if any.
///
/// An explicit `--root` or `ORBIT_ROOT` is a generation authority even when
/// it is not an initialized workspace. In that case this returns the
/// initialized workspace discovered from the working directory, rather than
/// the strict resolver's "not a workspace" error. When the explicit path
/// is initialized, the result matches `workspace_for_process`.
/// Malformed roots and broken workspace configs still fail.
pub fn workspace_for_preflight(
    root_override: Option<&Path>,
    env: &Env,
) -> Result<Option<UpdateWorkspace>, OrbitError> {
    let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
    let Some(explicit) = explicit_root_specification(root_override, env) else {
        return workspace_for_process(None, env);
    };
    match RegisteredRuntimeFactory::try_initialized_explicit_root(&cwd, &explicit)? {
        Some(_) => workspace_for_process(root_override, env),
        None => cwd_convergence_workspace(&cwd),
    }
}

/// Read this process's own installation, platform, and workspace.
pub fn from_process(
    root_override: Option<&Path>,
    env: &Env,
) -> Result<UpdateEnvironment, OrbitError> {
    let executable =
        converge::resolve_installed_executable(&std::env::current_exe().map_err(|error| {
            OrbitError::Io(format!("cannot locate the running orbit: {error}"))
        })?);
    let install_channel = InstallChannel::detect_with_homebrew_ownership(
        &executable,
        channel::managed_install_dir(env).as_deref(),
        &channel::SystemHomebrewInventory::system(),
    );
    assemble(
        root_override,
        executable,
        install_channel,
        orbit_core::app::update::trusted_keys_from_env(env)?,
        env,
    )
}

/// The environment for `--local-candidate`: the explicit `install_target`,
/// never this process's own executable — which is normally the candidate
/// itself, bootstrapping the update capability an older installed binary
/// lacks. A local candidate reads no published release, so it carries no
/// release trust.
pub fn for_install_target(
    root_override: Option<&Path>,
    install_target: &Path,
    env: &Env,
) -> Result<UpdateEnvironment, OrbitError> {
    let executable = if install_target.is_absolute() {
        install_target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| OrbitError::Io(error.to_string()))?
            .join(install_target)
    };
    let install_channel =
        InstallChannel::detect(&executable, channel::managed_install_dir(env).as_deref());
    assemble(root_override, executable, install_channel, &[], env)
}

fn assemble(
    root_override: Option<&Path>,
    executable: PathBuf,
    install_channel: InstallChannel,
    trusted_keys: &'static [TrustedReleaseKey],
    env: &Env,
) -> Result<UpdateEnvironment, OrbitError> {
    let workspace = workspace_for_process(root_override, env)?;
    let admission_roots = admission_authorities(
        root_override,
        workspace.as_ref().map(|workspace| workspace.root.as_path()),
    )?;
    Ok(UpdateEnvironment {
        admission_roots,
        executable,
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        target_triple: channel::release_target_triple()?.to_string(),
        install_channel,
        source: release_source_from_env(env),
        trusted_keys,
        today: chrono::Utc::now().date_naive(),
        workspace,
        bundled_bwrap_installed: cfg!(target_os = "linux")
            && std::fs::symlink_metadata(orbit_core::app::host::BUNDLED_BWRAP_PATH).is_ok(),
        probe_timeout: converge::probe_timeout_from_env(env),
    })
}

/// `--root` when present, otherwise a non-empty `ORBIT_ROOT`.
///
/// Matches the explicit-root precedence in `try_resolve_initialized_roots`:
/// the flag wins, and the environment value is returned untrimmed so path
/// resolution sees the same string the strict resolver does.
fn explicit_root_specification(root_override: Option<&Path>, env: &Env) -> Option<String> {
    if let Some(root) = root_override {
        return Some(root.to_string_lossy().into_owned());
    }
    env.orbit_root()
        .and_then(OsStr::to_str)
        .filter(|explicit| !explicit.trim().is_empty())
        .map(str::to_string)
}

/// Initialized workspace selected from `cwd`, ignoring an explicit generation root.
fn cwd_convergence_workspace(cwd: &Path) -> Result<Option<UpdateWorkspace>, OrbitError> {
    let roots = RegisteredRuntimeFactory::try_resolve_initialized_cwd_roots(cwd)?;
    Ok(roots.map(|roots| UpdateWorkspace {
        cwd: cwd.to_path_buf(),
        root: roots.shared_root,
        root_argument: None,
    }))
}
