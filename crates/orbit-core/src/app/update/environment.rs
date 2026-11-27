//! The installation, platform, and workspace an update runs against.

use super::channel::InstallChannel;
use super::source::ReleaseSource;
use chrono::NaiveDate;
use orbit_common::security::release::TrustedReleaseKey;
use std::path::PathBuf;
use std::time::Duration;

/// The machine facts an update runs against.
///
/// Constructed from the process by the caller; tests
/// build one directly so the whole flow runs against fixtures without touching
/// a real installation.
pub struct UpdateEnvironment {
    /// Generation-admission authorities for this invocation, in the order they
    /// are locked. Built by [`super::admission_authorities`]; `--preflight` reports
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
    /// Timeout applied to each `--version` and `update --contract` probe.
    pub probe_timeout: Duration,
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
