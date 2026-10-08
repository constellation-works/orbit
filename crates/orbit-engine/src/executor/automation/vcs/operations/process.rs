use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

/// Bounded attempts for a private automation PR lookup, including the first
/// try. `pr_open`'s existing-PR check must survive a single GitHub API blip
/// rather than abort a delivery whose branch is already pushed (F2026-09-011).
const GITHUB_LOOKUP_TRANSIENT_ATTEMPTS: u32 = 3;
const GITHUB_LOOKUP_TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Retry a private automation VCS read-only PR lookup (`PR_LIST`, `PR_VIEW`,
/// or `PR_STATUS`) across a bounded number of attempts when GitHub answers with
/// a transient failure. `pr_open` calls the first two operations to check for
/// an existing PR before deciding whether to create one; `PR_STATUS` is used
/// during completion. Pushes have their own remote-confirming retry path;
/// PR creation and merge are never retried here, since resending those mutations
/// after an ambiguous failure risks a duplicate side effect.
pub(super) fn execute_with_transient_retry(
    program: &str,
    args: &[String],
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let mut attempt = 1;
    loop {
        match execute(program, args.to_vec(), current_dir, timeout_ms, operation) {
            Ok(result) => return Ok(result),
            Err(error)
                if attempt < GITHUB_LOOKUP_TRANSIENT_ATTEMPTS
                    && is_transient_github_lookup_failure(&error.to_string()) =>
            {
                tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
                    operation,
                    attempt,
                    "retrying private automation VCS lookup after a transient GitHub failure"
                );
                std::thread::sleep(GITHUB_LOOKUP_TRANSIENT_RETRY_DELAY);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// True when a private automation VCS failure looks like a transient GitHub
/// gateway or GraphQL failure (502/503/504, a request timeout, or GitHub's
/// generic "Something went wrong while executing your query" response) rather
/// than a permanent failure such as auth, an unknown head, or an invalid
/// selector. Permanent failures must fail on the first attempt.
fn is_transient_github_lookup_failure(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("http 502")
        || text.contains("http 503")
        || text.contains("http 504")
        || text.contains("we couldn't respond to your request in time")
        || (text.contains("graphql")
            && text.contains("something went wrong while executing your query"))
        || (text.contains("graphql") && text.contains("timeout"))
}

pub(super) fn execute(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    succeeded(
        run_vcs_process(program, args, current_dir, timeout_ms)?,
        operation,
    )
}

/// Run one VCS process and return its outcome, failed or not.
pub(super) fn run_vcs_process(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let request = if program == "git" {
        let root = current_dir.ok_or_else(|| {
            OrbitError::InvalidInput("Git operation requires a working directory".to_string())
        })?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        super::super::git::git_request(root, &args, timeout_ms)
    } else {
        ExecRequest {
            program: program.to_string(),
            args,
            current_dir: current_dir.map(|path| path.to_string_lossy().into_owned()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(super::super::git::vcs_environment(&[
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
                "GH_HOST",
                "GH_CONFIG_DIR",
                "XDG_CONFIG_HOME",
            ])),
            debug: false,
        }
    };
    run_process(&request, &NoSandbox)
}

pub(super) fn succeeded(
    result: orbit_exec::ExecutionResult,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    if !result.success {
        return Err(OrbitError::Execution(format!(
            "private automation VCS {operation} failed: {}",
            result.stderr.trim()
        )));
    }
    Ok(result)
}
