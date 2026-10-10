//! The Claude worker-token readiness row of `orbit doctor` [ORB-15154].
//!
//! On macOS a `claude` started without `CLAUDE_CODE_OAUTH_TOKEN` (or
//! `ANTHROPIC_API_KEY`) falls back to the Claude Desktop app's shared login,
//! which the Desktop revokes on refresh and so fails the run with a 401. Two
//! places can lose the token: the workspace's effective `execution.env.pass`,
//! and the clock's environment, which launchd starts with no login shell and
//! which only gets it from `clock.env`. Values are never read into the row.

use std::path::{Path, PathBuf};

use orbit_common::security::operator_env::{CLOCK_ENV_FILE_NAME, clock_env_file_names};
use orbit_config::{ConfigRoots, ResolvedConfig};
use orbit_core::OrbitRuntime;
use orbit_core::application::routines::{ClockUnitVerdict, inspect_clock_unit};

use super::report::routed_crew_names;
use super::{WorkspaceDoctorResult, WorkspaceDoctorStatus};

const CHECK: &str = "claude-worker-token";
const OAUTH_TOKEN: &str = "CLAUDE_CODE_OAUTH_TOKEN";
const API_KEY: &str = "ANTHROPIC_API_KEY";
const CREDENTIALS: [&str; 2] = [OAUTH_TOKEN, API_KEY];

/// What the clock's environment file offers the ticks launchd starts.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ClockCredentials {
    /// No clock unit is installed, so no unattended tick needs the token.
    NoClock,
    /// `clock.env` holds a credential this workspace's pass admits.
    Provided,
    /// `clock.env` is absent or names no credential.
    Missing,
    /// `clock.env` holds credentials, but the workspace's pass admits none of
    /// them, so the tick exports nothing a run here would receive. Names only.
    Mismatched(Vec<String>),
    /// `clock.env` exists but the tick would refuse it.
    Refused(String),
}

/// The config file that admits a credential name for this workspace.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PassHome {
    /// No workspace policy file: the workspace inherits the global pass list.
    Global(PathBuf),
    /// The workspace's own policy file: its pass replaces the global list, so
    /// a global entry never reaches this workspace.
    Workspace(PathBuf),
}

impl PassHome {
    /// Names the key and file a fix edits to admit a name in this workspace.
    fn describe(&self) -> String {
        match self {
            Self::Global(path) => format!("`[execution.env] pass` in {}", path.display()),
            Self::Workspace(path) => format!(
                "`[execution.env] pass` in this workspace's {} (its pass replaces the global \
                 list, so restate every name the workspace needs there)",
                path.display()
            ),
        }
    }
}

pub(super) struct WorkerTokenFacts {
    pub macos: bool,
    pub claude_routed: bool,
    /// The workspace's effective `execution.env.pass`.
    pub pass: Vec<String>,
    pub clock: ClockCredentials,
    pub pass_home: PassHome,
}

pub(super) fn worker_token_row(facts: &WorkerTokenFacts) -> WorkspaceDoctorResult {
    let row = |status, message: String, remediation: Option<String>| WorkspaceDoctorResult {
        duration_ms: 0,
        check_name: CHECK.to_string(),
        status,
        message,
        remediation,
    };
    if !facts.macos {
        return row(
            WorkspaceDoctorStatus::Skipped,
            "only macOS shares a Claude Desktop login with the claude CLI".to_string(),
            None,
        );
    }
    if !facts.claude_routed {
        return row(
            WorkspaceDoctorStatus::Skipped,
            "no routed crew uses the claude provider".to_string(),
            None,
        );
    }
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    if !facts
        .pass
        .iter()
        .any(|name| CREDENTIALS.contains(&name.as_str()))
    {
        problems.push(format!(
            "`execution.env.pass` lists neither {OAUTH_TOKEN} nor {API_KEY}, so no Claude \
             activity receives it"
        ));
        fixes.push(format!(
            "Add {OAUTH_TOKEN} to {}.",
            facts.pass_home.describe()
        ));
    }
    match &facts.clock {
        ClockCredentials::NoClock | ClockCredentials::Provided => {}
        ClockCredentials::Missing => {
            problems.push(format!(
                "the clock's environment has no {OAUTH_TOKEN}: launchd runs `orbit clock tick` \
                 without your login shell and {CLOCK_ENV_FILE_NAME} does not provide it"
            ));
            fixes.push(format!(
                "Put {OAUTH_TOKEN}=<token from `claude setup-token`> in ~/.orbit/{CLOCK_ENV_FILE_NAME} \
                 (mode 600; `orbit routine init --install-clock` creates it)."
            ));
        }
        ClockCredentials::Mismatched(held) => {
            let held = held.join(", ");
            problems.push(format!(
                "the clock's environment holds {held}, but `execution.env.pass` does not admit \
                 it, so clock-started runs never receive it"
            ));
            fixes.push(format!(
                "Add {held} to {}, or put {OAUTH_TOKEN} or {API_KEY} in \
                 ~/.orbit/{CLOCK_ENV_FILE_NAME} under a name the pass admits.",
                facts.pass_home.describe()
            ));
        }
        ClockCredentials::Refused(reason) => {
            problems.push(format!(
                "the clock cannot use {CLOCK_ENV_FILE_NAME}: {reason}"
            ));
            fixes.push(format!(
                "Repair ~/.orbit/{CLOCK_ENV_FILE_NAME} (owner-only, regular file)."
            ));
        }
    }
    if problems.is_empty() {
        return row(
            WorkspaceDoctorStatus::Ok,
            "Claude activities receive a worker credential, including those the clock starts"
                .to_string(),
            None,
        );
    }
    fixes.push(
        "Without it claude falls back to the Claude Desktop login, which the Desktop revokes \
         mid-run (HTTP 401); Orbit refuses such Claude activities on macOS."
            .to_string(),
    );
    row(
        WorkspaceDoctorStatus::Warning,
        format!(
            "a routed crew can run claude on this macOS host, but {}",
            problems.join("; ")
        ),
        Some(fixes.join(" ")),
    )
}

/// Classifies the credentials `clock.env` holds against the workspace's
/// effective pass. The tick exports only names the pass admits, so a held
/// credential under a name the pass omits never reaches a run here.
pub(super) fn classify_clock_credentials(
    file: Result<Option<Vec<String>>, String>,
    pass: &[String],
) -> ClockCredentials {
    match file {
        Err(reason) => ClockCredentials::Refused(reason),
        Ok(None) => ClockCredentials::Missing,
        Ok(Some(held)) if held.is_empty() => ClockCredentials::Missing,
        Ok(Some(held)) if held.iter().any(|name| pass.contains(name)) => ClockCredentials::Provided,
        Ok(Some(held)) => ClockCredentials::Mismatched(held),
    }
}

/// The file that admits names for this workspace: its own policy config when
/// one sets policy, otherwise the global config (the same test `orbit config
/// show` applies to the security-key exception).
fn pass_home(runtime: &OrbitRuntime) -> PassHome {
    let shared_root = runtime.shared_root();
    let global_root = runtime.global_root();
    let workspace_config = shared_root.join("config.toml");
    if shared_root != global_root && orbit_config::workspace_config_sets_policy(&workspace_config) {
        PassHome::Workspace(workspace_config)
    } else {
        PassHome::Global(global_root.join("config.toml"))
    }
}

fn clock_credentials(global_root: &Path, pass: &[String]) -> ClockCredentials {
    let installed = inspect_clock_unit()
        .is_ok_and(|inspection| inspection.verdict != ClockUnitVerdict::NoUnitInstalled);
    if !installed {
        return ClockCredentials::NoClock;
    }
    let names: Vec<String> = CREDENTIALS.iter().map(|name| (*name).to_string()).collect();
    classify_clock_credentials(
        clock_env_file_names(global_root, &names).map_err(|error| error.to_string()),
        pass,
    )
}

/// The `claude-worker-token` row for this workspace and host.
pub(super) fn doctor_check_claude_worker_token(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let macos = cfg!(target_os = "macos");
    let global_root = runtime.global_root();
    let claude_routed = macos
        && ResolvedConfig::load(&ConfigRoots::new(&global_root, runtime.shared_root()))
            .ok()
            .is_some_and(|config| {
                routed_crew_names(&config).is_ok_and(|names| {
                    names.iter().any(|name| {
                        config
                            .crews
                            .get(name)
                            .is_some_and(|crew| crew.assignment.provider == "claude")
                    })
                })
            });
    let pass = runtime.env_pass_names();
    let clock = if macos {
        clock_credentials(&global_root, &pass)
    } else {
        ClockCredentials::NoClock
    };
    worker_token_row(&WorkerTokenFacts {
        macos,
        claude_routed,
        pass,
        clock,
        pass_home: pass_home(runtime),
    })
}
