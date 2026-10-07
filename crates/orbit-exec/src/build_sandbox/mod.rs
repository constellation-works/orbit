//! The plugin build profile: the sandbox an install-time `spec.build` phase
//! runs under (`docs/design/plugins/3_install_time_build.md` §3.2–§3.4).
//!
//! It is neither the agent profile (host readable, writes confined) nor the
//! backend profile (grants from the manifest). Reads are denied by default:
//! the child sees the host runtime table, the consented programs and
//! toolchain roots, and the build directory, which is also the only writable
//! path. The credential denies apply on top and always win.
//!
//! - **Linux** (`linux-bwrap-build-v1`): Bubblewrap with a constructed root,
//!   every namespace unshared, a fresh `/proc` and a minimal `/dev`. The
//!   `build` phase has no network namespace to speak of; `fetch` shares the
//!   host network under a Landlock ruleset that allows only outbound TCP to
//!   port 443.
//! - **macOS** (`macos-sandbox-build-v1`): a deny-default SBPL profile with
//!   no network. macOS runs no `fetch` phase ([`BUILD_FETCH_PHASE_SUPPORTED`]).
//!
//! A host that cannot apply the profile refuses the build: there is no
//! weaker fallback. [`run_build_phase`] supervises one phase: one capped log
//! for stdout and stderr, a wall-clock bound and a build-directory size bound,
//! either of which kills the whole process group.

mod linux;
mod macos;
mod supervise;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;

pub use linux::compile_linux_build_argv;
pub use macos::compile_macos_build_profile;
pub use supervise::{BuildLog, PLUGIN_BUILD_LOG_CAP_BYTES};

/// The size the build directory may reach before the phase is killed (§3.4).
pub const PLUGIN_BUILD_DIR_CAP_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// The only TCP port a `fetch` phase may connect to (§3.3).
pub const PLUGIN_BUILD_FETCH_PORT: u16 = 443;

/// Whether this platform runs a network `fetch` phase (§3.3). macOS does
/// not: without a pid namespace, a fetch descendant that calls `setsid`
/// escapes the process-group kill and keeps the fetch profile's network after
/// the phase ends. A manifest that declares `fetch` is refused there before
/// anything runs; an offline build still runs. (Platforms other than Linux
/// and macOS run no build at all: [`probe_build_sandbox`] refuses them.)
pub const BUILD_FETCH_PHASE_SUPPORTED: bool = cfg!(not(target_os = "macos"));

/// Whether a phase has a network (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPhaseNetwork {
    /// The `build` phase: no network.
    None,
    /// The `fetch` phase: outbound TCP to [`PLUGIN_BUILD_FETCH_PORT`] and name
    /// resolution, nothing else.
    Https,
}

/// What a phase's sandbox lets the child see.
#[derive(Debug, Clone, Copy)]
pub struct BuildSandboxSpec<'a> {
    /// The build directory: absolute, physical, and the only writable path.
    pub build_dir: &'a Path,
    /// Consented programs and toolchain roots, each absolute: readable and
    /// executable, never writable.
    pub readable: &'a [PathBuf],
    /// The consenting operator's `HOME`, whose credential trees stay denied
    /// even inside a readable root.
    pub home: Option<&'a OsStr>,
    pub network: BuildPhaseNetwork,
}

/// One phase to run.
pub struct BuildPhaseRequest<'a> {
    pub sandbox: BuildSandboxSpec<'a>,
    /// The rendered argv. `argv[0]` is an absolute program path.
    pub argv: &'a [String],
    /// The complete child environment.
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub timeout: Duration,
    /// [`PLUGIN_BUILD_DIR_CAP_BYTES`] outside tests.
    pub build_dir_cap_bytes: u64,
}

/// How a phase ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPhaseEnd {
    /// The program exited with this code.
    Exited(i32),
    /// The program was killed by this signal.
    Signaled(i32),
    /// The phase outlived its timeout and its process group was killed.
    TimedOut,
    /// The build directory exceeded its cap or could not be fully measured;
    /// the phase's process group was killed.
    BuildDirCapExceeded,
}

impl BuildPhaseEnd {
    pub fn succeeded(self) -> bool {
        self == Self::Exited(0)
    }
}

/// The profile this host can apply to a build, from [`probe_build_sandbox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSandboxProbe {
    /// The profile a build record names.
    pub profile: &'static str,
    /// The Landlock ABI a Linux `fetch` phase is confined by.
    pub landlock_abi: Option<i64>,
}

/// Whether this host can apply the build profile, for a manifest that does
/// (`fetch`) or does not declare a network phase. The error is the reason a
/// build must be refused.
pub fn probe_build_sandbox(fetch: bool) -> Result<BuildSandboxProbe, String> {
    #[cfg(target_os = "linux")]
    {
        linux::probe(fetch)
    }
    #[cfg(target_os = "macos")]
    {
        macos::probe(fetch)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = fetch;
        Err("plugin builds run only on Linux and macOS".to_string())
    }
}

/// Run one phase under the build profile, appending its output to `log`.
/// The caller probes first; a profile that cannot be applied here is an
/// error, never a weaker run.
pub fn run_build_phase(
    request: &BuildPhaseRequest<'_>,
    log: &mut BuildLog,
) -> Result<BuildPhaseEnd, OrbitError> {
    if !request.sandbox.build_dir.is_absolute() || !request.cwd.is_absolute() {
        return Err(OrbitError::InvalidInput(
            "a build phase needs an absolute build directory and working directory".to_string(),
        ));
    }
    let Some(program) = request.argv.first() else {
        return Err(OrbitError::InvalidInput(
            "a build phase needs a program".to_string(),
        ));
    };
    if !Path::new(program).is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "build program `{program}` must be resolved to an absolute path"
        )));
    }
    #[cfg(target_os = "linux")]
    {
        let (command, guard) = linux::command(request)?;
        supervise::run(command, guard, request, log)
    }
    #[cfg(target_os = "macos")]
    {
        let (command, guard) = macos::command(request)?;
        supervise::run(command, guard, request, log)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = log;
        Err(OrbitError::PolicyDenied(
            "plugin builds run only on Linux and macOS".to_string(),
        ))
    }
}

/// Credential denies apply even when a credential lives under the fixed
/// system runtime rather than one of the declared toolchain roots.
fn credential_denies(spec: &BuildSandboxSpec<'_>) -> Vec<(PathBuf, bool)> {
    let cargo_home = std::env::var_os("CARGO_HOME");
    crate::credential_paths::credential_read_denies(spec.home, cargo_home.as_deref())
        .into_iter()
        .map(|deny| (deny.path, deny.file))
        .collect()
}

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
