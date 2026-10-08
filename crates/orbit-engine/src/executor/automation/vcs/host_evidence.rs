//! Run one admitted `host_sandbox_test` command on this host, outside any
//! agent sandbox, at an exact candidate [ORB-14478].
//!
//! The command runs in a fresh detached worktree of the candidate commit, so
//! nothing the checkout holds beyond that commit can reach it and nothing the
//! run writes reaches the checkout. Its Cargo target directory is the one the
//! caller names unless the validation environment names one: a claimed leaf
//! shares its checkout's, so a single test target does not rebuild every
//! dependency; a Linux owner fulfilling a hold [ORB-14334] uses one of its
//! own, never the operator checkout's. On macOS
//! `ORBIT_REQUIRE_SANDBOX_EXEC=1` turns a Seatbelt test's visible skip into a
//! failure, which the judgement then names as the host's condition.
//!
//! What counts as passing evidence is
//! [`judge_host_test_output`](orbit_types::workflow::judge_host_test_output),
//! read over the whole output before the log copy is bounded.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::git::git_common_dir;
use orbit_common::security::release::sha256_hex;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    HostEvidenceReason, HostEvidenceRefusal, HostSandboxCommand, judge_host_test_output,
};
use serde_json::Value;

use crate::context::RuntimeHost;

use super::baseline::in_detached_worktree;
use super::required_command::{VALIDATION_TIMEOUT_MS, capture, environment_record, missing_tool};

/// Directory under the Git common directory holding host-evidence worktrees.
const WORKTREE_DIR: &str = "orbit-host-evidence";
/// The switch macOS sandbox tests read to fail rather than skip.
const REQUIRE_SANDBOX_EXEC: &str = "ORBIT_REQUIRE_SANDBOX_EXEC";

/// One host run and its judgement.
#[derive(Debug, Clone)]
pub struct HostEvidenceRun {
    /// The command line the host ran; empty when it ran nothing.
    pub host_command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// What the command wrote, bounded for the log.
    pub output: String,
    /// How the validation environment was resolved, when the command ran.
    pub environment: Value,
    /// Passed tests counted from the libtest summaries, or why the run is
    /// not passing evidence.
    pub judgement: Result<u64, HostEvidenceRefusal>,
}

impl HostEvidenceRun {
    fn refused(reason: HostEvidenceReason, detail: String) -> Self {
        Self {
            host_command: String::new(),
            exit_code: None,
            timed_out: false,
            output: String::new(),
            environment: Value::Null,
            judgement: Err(HostEvidenceRefusal { reason, detail }),
        }
    }
}

/// Run `command` at `candidate`, in a worktree of `workspace_path`'s
/// repository building into `target_dir`, and judge it. Only call this with a
/// command [`HostSandboxCommand::admit`] admitted: it runs on the host,
/// outside any agent sandbox.
pub fn run_host_sandbox_test<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    candidate: &SourceRevision,
    command: &HostSandboxCommand,
    run_id: &str,
    target_dir: &Path,
) -> Result<HostEvidenceRun, OrbitError> {
    match super::review_gate::revision(workspace_path, &candidate.commit) {
        Ok(found) if found == *candidate => {}
        Ok(found) => {
            return Ok(HostEvidenceRun::refused(
                HostEvidenceReason::CandidateChanged,
                format!(
                    "commit {} has tree {}, not the held {}",
                    candidate.commit, found.tree, candidate.tree
                ),
            ));
        }
        Err(error) => {
            return Ok(HostEvidenceRun::refused(
                HostEvidenceReason::CandidateChanged,
                error.to_string(),
            ));
        }
    }
    let host_command = command.host_command();
    let key = sha256_hex(format!("{run_id}\0{}\0{host_command}", candidate.commit).as_bytes());
    let worktree = git_common_dir(workspace_path)?
        .join(WORKTREE_DIR)
        .join(&key[..16]);
    let ran = in_detached_worktree(workspace_path, &worktree, &candidate.commit, |checkout| {
        execute(host, checkout, target_dir, command, &host_command)
    });
    Ok(match ran {
        Ok(run) => run,
        Err(error) => HostEvidenceRun::refused(
            HostEvidenceReason::CandidateChanged,
            format!("check out {}: {error}", candidate.commit),
        ),
    })
}

fn execute<H: RuntimeHost + ?Sized>(
    host: &H,
    checkout: &Path,
    shared_target: &Path,
    command: &HostSandboxCommand,
    host_command: &str,
) -> HostEvidenceRun {
    let mut environment = host.validation_subprocess_environment();
    if !environment
        .env
        .iter()
        .any(|(name, value)| name == "CARGO_TARGET_DIR" && !value.is_empty())
    {
        environment.env.push((
            "CARGO_TARGET_DIR".to_string(),
            shared_target.to_string_lossy().into_owned(),
        ));
    }
    if cfg!(target_os = "macos") {
        environment
            .env
            .retain(|(name, _)| name != REQUIRE_SANDBOX_EXEC);
        environment
            .env
            .push((REQUIRE_SANDBOX_EXEC.to_string(), "1".to_string()));
    }
    let record = environment_record(&environment);
    // Every word is admitted from a set with no shell meaning, so the shell
    // only resolves the program on the validation PATH.
    let outcome = run_process(
        &ExecRequest {
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), host_command.to_string()],
            current_dir: Some(checkout.to_string_lossy().into_owned()),
            timeout_ms: Some(VALIDATION_TIMEOUT_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(environment.env.clone()),
            debug: false,
        },
        &NoSandbox,
    );
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            let mut run =
                HostEvidenceRun::refused(HostEvidenceReason::RunFailed, error.to_string());
            run.host_command = host_command.to_string();
            run.environment = record;
            return run;
        }
    };
    let full = format!("{}\n{}", outcome.stdout, outcome.stderr);
    let passed = outcome.success && !outcome.timed_out;
    let judgement = match (!passed && !outcome.timed_out)
        .then(|| missing_tool(outcome.exit_code, &full, environment.path().unwrap_or("")))
        .flatten()
    {
        Some(missing) => Err(HostEvidenceRefusal {
            reason: HostEvidenceReason::ToolMissing,
            detail: missing.evidence,
        }),
        None => judge_host_test_output(command, passed, outcome.timed_out, &full),
    };
    HostEvidenceRun {
        host_command: host_command.to_string(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        output: capture(&outcome.stdout, &outcome.stderr),
        environment: record,
        judgement,
    }
}
