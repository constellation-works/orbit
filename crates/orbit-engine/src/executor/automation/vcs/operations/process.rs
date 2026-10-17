use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

/// Bounded attempts for private automation PR lookup and reconciled creation,
/// including the first try. `pr_open` must survive a single GitHub API blip
/// rather than abort a delivery whose branch is already pushed (F2026-09-011).
pub(in crate::executor::automation::vcs) const GITHUB_TRANSIENT_ATTEMPTS: u32 = 3;
pub(in crate::executor::automation::vcs) const GITHUB_TRANSIENT_RETRY_DELAY: Duration =
    Duration::from_millis(500);

/// Retry a private automation VCS read-only PR lookup (`PR_LIST`,
/// `PR_LIST_OPEN`, `PR_VIEW`, or `PR_STATUS`) across a bounded number of
/// attempts when GitHub answers with a transient failure. `pr_open` calls
/// `PR_LIST` and `PR_VIEW` to check for an existing PR before deciding whether
/// to create one, and `PR_LIST_OPEN` to find the tasks' earlier PRs once it
/// created one; `PR_STATUS` is used during completion. Pushes have their own remote-confirming retry path;
/// PR creation and merge are never retried in this process wrapper: `pr_open`
/// reconciles ambiguous create failures by looking up the head before retrying,
/// while resending a merge risks a duplicate side effect.
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
                if attempt < GITHUB_TRANSIENT_ATTEMPTS
                    && is_transient_github_failure(&error.to_string()) =>
            {
                tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
                    operation,
                    attempt,
                    "retrying private automation VCS lookup after a transient GitHub failure"
                );
                super::super::remote_probe::fixed_wait(GITHUB_TRANSIENT_RETRY_DELAY);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// True when a private automation VCS failure looks like a transient GitHub
/// gateway, REST transport, or GraphQL failure: any three-digit HTTP 5xx, a
/// TLS handshake timeout (`net/http: TLS handshake timeout` or
/// `tls: handshake timeout`) even when the URL is not GraphQL, a request
/// timeout, or GitHub's generic "Something went wrong while executing your
/// query" response. Permanent failures such as auth, an unknown head, or an
/// invalid selector must fail on the first attempt. [ORB-16340]
pub(in crate::executor::automation::vcs) fn is_transient_github_failure(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    transient_http_5xx(&text)
        || text.contains("we couldn't respond to your request in time")
        || (text.contains("graphql")
            && text.contains("something went wrong while executing your query"))
        || (text.contains("graphql") && text.contains("timeout"))
        || text.contains("tls handshake timeout")
        || text.contains("tls: handshake timeout")
}

/// `HTTP 500` through `HTTP 599`. A longer token (`http 5000`) and a URL
/// (`https://`) do not match.
fn transient_http_5xx(text: &str) -> bool {
    text.match_indices("http ").any(|(index, prefix)| {
        let status = &text[index + prefix.len()..];
        let bytes = status.as_bytes();
        bytes.len() >= 3
            && bytes[0] == b'5'
            && bytes[1].is_ascii_digit()
            && bytes[2].is_ascii_digit()
            && bytes.get(3).is_none_or(|byte| !byte.is_ascii_digit())
    })
}

/// True when a failed read-only provider observation is a transport failure.
/// A credential, permission, permanent HTTP, malformed-answer or deadline
/// diagnostic anywhere in the message disqualifies it, as does unreadable,
/// truncated or changed prerequisite evidence. TLS and HTTP 5xx recognition
/// lives in [`is_transient_github_failure`], so a REST handshake timeout is
/// not limited to the completion-observation wrapper.
pub(in crate::executor::automation::vcs) fn is_transient_observation_failure(
    message: &str,
) -> bool {
    let text = message.to_ascii_lowercase();
    is_transient_github_failure(&text)
        && ![
            "http 400",
            "http 401",
            "http 403",
            "http 404",
            "http 409",
            "http 410",
            "http 422",
            "http 429",
            "bad credentials",
            "permission",
            "not accessible by integration",
            "authentication",
            "saml",
            "rate limit",
            "invalid json",
            "invalid api json",
            "evidence is unavailable (",
            "deadline expired",
        ]
        .iter()
        .any(|token| text.contains(token))
}

pub(in crate::executor::automation::vcs) fn execute(
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
