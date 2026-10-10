//! The Claude worker-token readiness row of `orbit doctor` [ORB-15154].
//!
//! On macOS a `claude` started without `CLAUDE_CODE_OAUTH_TOKEN` (or
//! `ANTHROPIC_API_KEY`) falls back to the Claude Desktop app's shared login,
//! which the Desktop revokes on refresh and so fails the run with a 401. Two
//! places can lose the token: the workspace's effective `execution.env.pass`,
//! and the clock's environment, which launchd starts with no login shell and
//! which only gets it from `clock.env`. Values are never read into the row.

use std::path::Path;

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
    /// `clock.env` holds a credential.
    Provided,
    /// `clock.env` is absent or names no credential.
    Missing,
    /// `clock.env` exists but the tick would refuse it.
    Refused(String),
}

pub(super) struct WorkerTokenFacts {
    pub macos: bool,
    pub claude_routed: bool,
    /// The workspace's effective `execution.env.pass`.
    pub pass: Vec<String>,
    pub clock: ClockCredentials,
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
            "Add {OAUTH_TOKEN} to `[execution.env] pass` in the global or workspace config.toml."
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

fn clock_credentials(global_root: &Path) -> ClockCredentials {
    let installed = inspect_clock_unit()
        .is_ok_and(|inspection| inspection.verdict != ClockUnitVerdict::NoUnitInstalled);
    if !installed {
        return ClockCredentials::NoClock;
    }
    let names: Vec<String> = CREDENTIALS.iter().map(|name| (*name).to_string()).collect();
    match clock_env_file_names(global_root, &names) {
        Ok(Some(held)) if !held.is_empty() => ClockCredentials::Provided,
        Ok(_) => ClockCredentials::Missing,
        Err(error) => ClockCredentials::Refused(error.to_string()),
    }
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
    worker_token_row(&WorkerTokenFacts {
        macos,
        claude_routed,
        pass: runtime.env_pass_names(),
        clock: if macos {
            clock_credentials(&global_root)
        } else {
            ClockCredentials::NoClock
        },
    })
}
