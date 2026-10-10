//! The installation, platform, and workspace an update runs against.

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::OrbitError;
use orbit_common::security::release::TrustedReleaseKey;

use super::admission::admission_authorities;
use super::channel::{self, InstallChannel};
use super::converge;
use super::source::{ReleaseSource, release_source_from_env};
use crate::registry_runtime::RegisteredRuntimeFactory;

/// The machine facts an update runs against.
///
/// Constructed from the process by [`UpdateEnvironment::from_process`]; tests
/// build one directly so the whole flow runs against fixtures without touching
/// a real installation.
pub struct UpdateEnvironment {
    /// Generation-admission authorities for this invocation, in the order they
    /// are locked. Built by [`admission_authorities`]; `--preflight` reports
    /// the same list.
    pub admission_roots: Vec<PathBuf>,
    /// The executable to replace.
    pub executable: PathBuf,
    /// Process snapshot of the running binary's version (`CARGO_PKG_VERSION`
    /// in production). `--check` uses this; mutation uses the on-disk
    /// version re-read under the install lock.
    pub current_version: String,
    /// Release target triple for this platform.
    pub target_triple: String,
    /// Which installer owns [`Self::executable`].
    pub install_channel: InstallChannel,
    /// Where release artifacts are read from.
    pub source: Box<dyn ReleaseSource>,
    /// Release signing keys to accept. The compiled set, unless
    /// `ORBIT_RELEASE_TRUSTED_KEYS_FILE` is set and acknowledged.
    pub trusted_keys: &'static [TrustedReleaseKey],
    /// Today's date, for signing-key expiry.
    pub today: NaiveDate,
    /// The initialized Orbit workspace selected for convergence, if any.
    pub workspace: Option<UpdateWorkspace>,
    /// Whether this Linux host runs the Bubblewrap bundled with Orbit, which
    /// the update then refreshes along with the executable.
    pub bundled_bwrap_installed: bool,
}

/// The process location and resolved root that update subprocesses must retain.
pub struct UpdateWorkspace {
    /// Working directory inherited by the replacement executable.
    pub cwd: PathBuf,
    /// Authoritative `.orbit` root selected by `--root`, `ORBIT_ROOT`, or cwd discovery.
    pub root: PathBuf,
    /// Root argument to forward when selection was explicit rather than cwd-based.
    pub root_argument: Option<PathBuf>,
}

impl UpdateEnvironment {
    /// Resolve the initialized workspace this process's convergence steps use.
    ///
    /// An explicit `--root` or `ORBIT_ROOT` must already be an initialized
    /// workspace. `--preflight` uses [`Self::workspace_for_preflight`] so it
    /// can probe an uninitialized generation root without this requirement.
    pub fn workspace_for_process(
        root_override: Option<&Path>,
    ) -> Result<Option<UpdateWorkspace>, OrbitError> {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        let root_was_explicit = root_override.is_some()
            || std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
        let roots = RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?;
        Ok(roots.map(|roots| UpdateWorkspace {
            cwd,
            root_argument: root_was_explicit.then(|| roots.shared_root.clone()),
            root: roots.shared_root,
        }))
    }

    /// Workspace whose convergence `--preflight` should admit, if any.
    ///
    /// An explicit `--root` or `ORBIT_ROOT` is a generation authority even when
    /// it is not an initialized workspace. In that case this returns the
    /// initialized workspace discovered from the working directory, rather than
    /// the strict resolver's "not a workspace" error. When the explicit path
    /// is initialized, the result matches [`Self::workspace_for_process`].
    /// Malformed roots and broken workspace configs still fail.
    pub fn workspace_for_preflight(
        root_override: Option<&Path>,
    ) -> Result<Option<UpdateWorkspace>, OrbitError> {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        let Some(explicit) = explicit_root_specification(root_override) else {
            return Self::workspace_for_process(None);
        };
        match RegisteredRuntimeFactory::try_initialized_explicit_root(&cwd, &explicit)? {
            Some(_) => Self::workspace_for_process(root_override),
            None => cwd_convergence_workspace(&cwd),
        }
    }

    /// Read this process's own installation, platform, and workspace.
    pub fn from_process(root_override: Option<&Path>) -> Result<Self, OrbitError> {
        let executable =
            converge::resolve_installed_executable(&std::env::current_exe().map_err(|error| {
                OrbitError::Io(format!("cannot locate the running orbit: {error}"))
            })?);
        let install_channel = InstallChannel::detect_with_homebrew_ownership(
            &executable,
            channel::managed_install_dir().as_deref(),
            &channel::SystemHomebrewInventory::system(),
        );
        Self::assemble(
            root_override,
            executable,
            install_channel,
            super::trust::trusted_keys_from_env()?,
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
    ) -> Result<Self, OrbitError> {
        let executable = if install_target.is_absolute() {
            install_target.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| OrbitError::Io(error.to_string()))?
                .join(install_target)
        };
        let install_channel =
            InstallChannel::detect(&executable, channel::managed_install_dir().as_deref());
        Self::assemble(root_override, executable, install_channel, &[])
    }

    fn assemble(
        root_override: Option<&Path>,
        executable: PathBuf,
        install_channel: InstallChannel,
        trusted_keys: &'static [TrustedReleaseKey],
    ) -> Result<Self, OrbitError> {
        let workspace = Self::workspace_for_process(root_override)?;
        Ok(Self {
            admission_roots: admission_authorities(
                root_override,
                workspace.as_ref().map(|workspace| workspace.root.as_path()),
            )?,
            install_channel,
            executable,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            target_triple: channel::release_target_triple()?.to_string(),
            source: release_source_from_env(),
            trusted_keys,
            today: chrono::Utc::now().date_naive(),
            workspace,
            bundled_bwrap_installed: cfg!(target_os = "linux")
                && std::fs::symlink_metadata(
                    orbit_core::bootstrap::linux_sandbox_host::BUNDLED_BWRAP_PATH,
                )
                .is_ok(),
        })
    }
}

/// `--root` when present, otherwise a non-empty `ORBIT_ROOT`.
///
/// Matches the explicit-root precedence in `try_resolve_initialized_roots`:
/// the flag wins, and the environment value is returned untrimmed so path
/// resolution sees the same string the strict resolver does.
fn explicit_root_specification(root_override: Option<&Path>) -> Option<String> {
    if let Some(root) = root_override {
        return Some(root.to_string_lossy().into_owned());
    }
    match std::env::var("ORBIT_ROOT") {
        Ok(explicit) if !explicit.trim().is_empty() => Some(explicit),
        _ => None,
    }
}

/// Initialized workspace selected from `cwd`, ignoring an explicit generation root.
fn cwd_convergence_workspace(cwd: &Path) -> Result<Option<UpdateWorkspace>, OrbitError> {
    let roots = RegisteredRuntimeFactory::try_resolve_initialized_cwd_roots(cwd)?;
    Ok(roots.map(|roots| UpdateWorkspace {
        cwd: cwd.to_path_buf(),
        root_argument: None,
        root: roots.shared_root,
    }))
}
