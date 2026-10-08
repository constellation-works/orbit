//! Run a consented plugin build under the build profile.

use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_exec::{
    BuildLog, BuildPhaseEnd, BuildPhaseNetwork, BuildPhaseRequest, BuildSandboxSpec,
    PLUGIN_BUILD_DIR_CAP_BYTES, probe_build_sandbox, run_build_phase,
};
use orbit_types::plugin::render_build_argv;

use super::dir::{PluginBuildDir, copy_checkout, create_build_dir};
use super::env::plugin_build_environment;
use super::plan::{PluginBuildPlan, quote_argv, recheck_build_paths};

/// Which phase an environment is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginBuildPhase {
    Fetch,
    Build,
}

impl PluginBuildPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Build => "build",
        }
    }
}

/// What one build produced, ready to be copied into a staging tree.
#[derive(Debug)]
pub struct PluginBuildResult {
    pub dir: PluginBuildDir,
    pub profile: String,
    pub landlock_abi: Option<i64>,
    /// Each phase argv exactly as run.
    pub fetch: Option<Vec<String>>,
    pub command: Vec<String>,
}

/// Inputs to [`run_plugin_build`].
pub struct PluginBuildRun<'a> {
    pub plan: &'a PluginBuildPlan,
    /// The pristine checkout; copied, never handed to the sandbox.
    pub checkout: &'a Path,
    /// `~/.orbit/plugins/<ns>/`.
    pub namespace_dir: &'a Path,
    /// Where the capped log is written, whatever the outcome.
    pub log_path: &'a Path,
    pub home: Option<&'a OsStr>,
}

/// Create the build directory, run `fetch` (when declared) and `build`
/// under the build profile, and keep the log. Refuses before creating
/// anything when this host cannot apply the profile.
pub fn run_plugin_build(run: &PluginBuildRun<'_>) -> Result<PluginBuildResult, OrbitError> {
    let plan = run.plan;
    let probe = probe_build_sandbox(plan.fetch.is_some()).map_err(|reason| {
        OrbitError::PolicyDenied(format!(
            "this host cannot apply the plugin build profile: {reason}. There is no weaker \
             fallback; nothing was built"
        ))
    })?;
    recheck_build_paths(plan)?;
    let dir = create_build_dir(run.namespace_dir)?;
    let build_dir = dir.path().to_path_buf();
    copy_checkout(run.checkout, &build_dir.join("src"))?;

    let readable = plan.readable();
    let mut log = BuildLog::default();
    log.note(&format!(
        "build of {} at {} under {}",
        plan.source, plan.commit, probe.profile
    ));
    let build_dir_text = build_dir.display().to_string();
    let mut phases = Vec::new();
    if let Some(fetch) = &plan.fetch {
        phases.push((PluginBuildPhase::Fetch, fetch, BuildPhaseNetwork::Https));
    }
    phases.push((
        PluginBuildPhase::Build,
        &plan.command,
        BuildPhaseNetwork::None,
    ));
    let mut ran: Vec<Vec<String>> = Vec::new();
    let mut failure = None;
    for (phase, argv, network) in phases {
        if let Err(error) = recheck_build_paths(plan) {
            failure = Some(format!("the {} phase was refused: {error}", phase.name()));
            break;
        }
        let mut rendered = render_build_argv(argv, &build_dir_text);
        if let Some(first) = rendered.first_mut()
            && let Some(program) = plan.programs.iter().find(|program| program.name == *first)
        {
            *first = program.invoked.display().to_string();
        }
        log.note(&format!(
            "phase {}: {}",
            phase.name(),
            quote_argv(&rendered)
        ));
        let env = plugin_build_environment(plan, &build_dir, phase);
        let request = BuildPhaseRequest {
            sandbox: BuildSandboxSpec {
                build_dir: &build_dir,
                readable: &readable,
                home: run.home,
                network,
            },
            argv: &rendered,
            env: &env,
            cwd: &build_dir.join("src"),
            timeout: Duration::from_millis(plan.timeout_ms),
            build_dir_cap_bytes: PLUGIN_BUILD_DIR_CAP_BYTES,
        };
        let end = run_build_phase(&request, &mut log);
        ran.push(rendered);
        match end {
            Ok(end) if end.succeeded() => log.note(&format!("phase {} succeeded", phase.name())),
            Ok(end) => {
                let reason = phase_failure(end, plan.timeout_ms);
                log.note(&format!("phase {} failed: {reason}", phase.name()));
                failure = Some(format!("the {} phase {reason}", phase.name()));
                break;
            }
            Err(error) => {
                log.note(&format!("phase {} could not run: {error}", phase.name()));
                failure = Some(format!("the {} phase could not run: {error}", phase.name()));
                break;
            }
        }
    }
    write_log(run.log_path, &log)?;
    if let Some(failure) = failure {
        return Err(OrbitError::Execution(format!(
            "plugin build failed: {failure}; log: {}. Nothing was installed",
            run.log_path.display()
        )));
    }
    let command = ran.pop().unwrap_or_default();
    Ok(PluginBuildResult {
        dir,
        profile: probe.profile.to_string(),
        landlock_abi: probe.landlock_abi,
        fetch: ran.pop(),
        command,
    })
}

fn phase_failure(end: BuildPhaseEnd, timeout_ms: u64) -> String {
    match end {
        BuildPhaseEnd::Exited(code) => format!("exited with status {code}"),
        BuildPhaseEnd::Signaled(signal) => format!("was killed by signal {signal}"),
        BuildPhaseEnd::TimedOut => format!("ran past its {} s timeout", timeout_ms / 1000),
        BuildPhaseEnd::BuildDirCapExceeded => format!(
            "exceeded the {} GiB build directory cap or could not be measured",
            PLUGIN_BUILD_DIR_CAP_BYTES / (1024 * 1024 * 1024)
        ),
    }
}

fn write_log(path: &Path, log: &BuildLog) -> Result<(), OrbitError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
    }
    atomic_write_bytes(path, &log.render())
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}
