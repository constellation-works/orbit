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
    /// Read this process's own installation, platform, and workspace.
    pub fn from_process(root_override: Option<&Path>) -> Result<Self, OrbitError> {
        let executable =
            converge::resolve_installed_executable(&std::env::current_exe().map_err(|error| {
                OrbitError::Io(format!("cannot locate the running orbit: {error}"))
            })?);
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        let root_was_explicit = root_override.is_some()
            || std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
        let workspace =
            RegisteredRuntimeFactory::try_resolve_initialized_roots(&cwd, root_override)?.map(
                |roots| UpdateWorkspace {
                    cwd,
                    root_argument: root_was_explicit.then(|| roots.shared_root.clone()),
                    root: roots.shared_root,
                },
            );
        Ok(Self {
            admission_roots: admission_authorities(root_override)?,
            install_channel: InstallChannel::detect_with_homebrew_ownership(
                &executable,
                channel::managed_install_dir().as_deref(),
                &channel::SystemHomebrewInventory::system(),
            ),
            executable,
            current_version: env!("CARGO_PKG_VERSION").to_string(),
            target_triple: channel::release_target_triple()?.to_string(),
            source: release_source_from_env(),
            trusted_keys: super::trust::trusted_keys_from_env()?,
            today: chrono::Utc::now().date_naive(),
            workspace,
        })
    }
}
