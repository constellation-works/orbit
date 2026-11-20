use std::path::Path;
use std::time::Duration;

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

/// Bounded attempts for private automation PR lookup and reconciled creation,
/// including the first try. `pr_open` must survive a single GitHub API blip
/// rather than abort a delivery whose branch is already pushed (F2026-09-011).
pub(in crate::executor::automation::vcs) const GITHUB_TRANSIENT_ATTEMPTS: u32 = 3;
pub(in crate::executor::automation::vcs) const GITHUB_TRANSIENT_RETRY_DELAY: Duration =
    Duration::from_millis(500);

/// Retry a private automation VCS lookup or idempotent label mutation across a bounded number
/// of attempts when GitHub answers with a transient failure. This includes
/// PR lists, views and status, repository identity and merge capabilities,
/// and CI and check-annotation reads without a completion deadline. Task-derived
/// label create and add operations are idempotent: create accepts an existing
/// label and edit only adds labels, so an ambiguous response can be retried.
/// Deadline-bounded reads retain their caller's observation recovery.
/// `pr_open` calls
/// `PR_LIST` and `PR_VIEW` to check for an existing PR before deciding whether
/// to create one, and `PR_LIST_OPEN` to find the tasks' earlier PRs once it
/// created one; `PR_STATUS`, repository identity and merge capabilities are
/// used during completion and landing. Pushes have their own remote-confirming retry path;
/// PR creation and merge are never retried in this process wrapper: `pr_open`
/// reconciles ambiguous create failures by looking up the head before retrying,
/// while resending a merge risks a duplicate side effect. A failed read keeps
/// GitHub's authenticated rate limit typed ([`succeeded_read`]), so the
/// caller's policy can wait for the reset.
pub(super) fn execute_with_transient_retry(
    program: &str,
    args: &[String],
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let mut attempt = 1;
    loop {
        let result = run_vcs_process(program, args.to_vec(), current_dir, timeout_ms)
            .and_then(|result| succeeded_read(result, operation));
        match result {
            Ok(result) => return Ok(result),
            Err(error) if attempt < GITHUB_TRANSIENT_ATTEMPTS && is_transient(&error) => {
                tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
                    operation,
                    attempt,
                    "retrying private automation VCS operation after a transient GitHub failure"
                );
                super::super::remote_probe::fixed_wait(GITHUB_TRANSIENT_RETRY_DELAY);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Whether `error` is a private automation VCS failure the provider reported
/// as transient.
pub(in crate::executor::automation::vcs) fn is_transient(error: &OrbitError) -> bool {
    error
        .private_vcs_failure()
        .is_some_and(|failure| failure.class.is_transient())
}

/// `wrapper`, an Orbit diagnostic that reports `cause`, preserving a typed
/// delivery refusal or the class of a private VCS failure. Any other wrapper
/// is returned unchanged: an Orbit refusal is never a transient provider
/// failure.
pub(in crate::executor::automation::vcs) fn carry_class(
    cause: &OrbitError,
    wrapper: OrbitError,
) -> OrbitError {
    match (cause.private_vcs_failure(), wrapper) {
        (Some(failure), OrbitError::Execution(message)) => {
            OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
                operation: failure.operation.clone(),
                class: failure.class,
                message,
                read_throttle: failure.read_throttle.clone(),
            }))
        }
        (_, OrbitError::Execution(message)) => match cause {
            OrbitError::DeliveryRefused { refusal, .. } => OrbitError::DeliveryRefused {
                refusal: *refusal,
                message,
            },
            _ => OrbitError::Execution(message),
        },
        (_, wrapper) => wrapper,
    }
}

/// The class of one failed private automation VCS process, read from the
/// diagnostic `succeeded` renders for it.
///
/// Transient: HTTP 499, any three-digit HTTP 5xx, a TLS handshake timeout
/// (`net/http: TLS handshake timeout` or `tls: handshake timeout`), a
/// connection the peer reset or one `gh` could not open (`error connecting to
/// api.github.com`) [ORB-19430], even when the URL is not GraphQL, a request
/// timeout, or GitHub's generic "Something
/// went wrong while executing your query" response. Permanent failures such
/// as auth, an unknown head, or an invalid selector must fail on the first
/// attempt [ORB-16340]. A transient failure is an observation failure unless
/// a credential, permission, rate-limit or permanent-HTTP diagnostic appears
/// anywhere in it. Orbit's own refusals never reach this text: they stay
/// [`OrbitError::Execution`], which is never transient.
fn classify(message: &str) -> PrivateVcsFailureClass {
    let text = message.to_ascii_lowercase();
    let transient = transient_http_499(&text)
        || transient_http_5xx(&text)
        || text.contains("we couldn't respond to your request in time")
        || (text.contains("graphql")
            && text.contains("something went wrong while executing your query"))
        || (text.contains("graphql") && text.contains("timeout"))
        || text.contains("tls handshake timeout")
        || text.contains("tls: handshake timeout")
        || text.contains("connection reset by peer")
        || text.contains("error connecting to ");
    if !transient {
        PrivateVcsFailureClass::Other
    } else if [
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
    ]
    .iter()
    .any(|token| text.contains(token))
    {
        PrivateVcsFailureClass::Transient
    } else {
        PrivateVcsFailureClass::TransientObservation
    }
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

/// `HTTP 499`. A longer token (`http 4990`) and a URL fragment do not match.
fn transient_http_499(text: &str) -> bool {
    text.match_indices("http ").any(|(index, prefix)| {
        let status = &text[index + prefix.len()..];
        let bytes = status.as_bytes();
        bytes.starts_with(b"499") && bytes.get(3).is_none_or(|byte| !byte.is_ascii_digit())
    })
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

fn build_vcs_request(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
) -> Result<ExecRequest, OrbitError> {
    if program == "git" {
        let root = current_dir.ok_or_else(|| {
            OrbitError::InvalidInput("Git operation requires a working directory".to_string())
        })?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        Ok(super::super::git::git_request(root, &args, timeout_ms))
    } else {
        Ok(ExecRequest {
            program: program.to_string(),
            args,
            current_dir: current_dir.map(|path| path.to_string_lossy().into_owned()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(super::super::git::gh_environment()),
            debug: false,
        })
    }
}

/// Run one VCS process and return its outcome, failed or not.
///
/// A `gh` call is delivery's spend of the host's GitHub quota: it is admitted
/// against the host record first, so a hold after a rate-limit answer refuses
/// it locally with a typed throttle and nothing runs, and its result is
/// recorded so a rate-limit answer holds every other reader on the host.
pub(in crate::executor::automation::vcs) fn run_vcs_process(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let request = build_vcs_request(program, args, current_dir, timeout_ms)?;
    if program == "git" {
        return run_process(&request, &NoSandbox);
    }
    orbit_tools::github_quota::run_gh_request(
        orbit_tools::github_quota::GithubCaller::Delivery,
        &request,
    )
}

/// [`run_vcs_process`] for a deadline-bounded caller: admission sees
/// `timeout_ms`, and `remaining_after_admission` validates and bounds the
/// remaining budget (both elapsed wall clock and any caller clock) before the
/// original `gh` process may spawn.
pub(in crate::executor::automation::vcs) fn run_vcs_process_within(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
    remaining_after_admission: impl FnOnce(u64) -> Result<u64, OrbitError>,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let mut request = build_vcs_request(program, args, current_dir, timeout_ms)?;
    if program == "git" {
        return run_process(&request, &NoSandbox);
    }
    let admit_started = std::time::Instant::now();
    let admission = orbit_tools::github_quota::admit_gh_request(
        orbit_tools::github_quota::GithubCaller::Delivery,
        &request,
    );
    let admit_elapsed_ms = u64::try_from(admit_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let remaining_ms = remaining_after_admission(admit_elapsed_ms)?;
    admission?;
    request.timeout_ms = Some(remaining_ms);
    let result = run_process(&request, &NoSandbox)?;
    orbit_tools::github_quota::observe_gh_result(&request, &result);
    Ok(result)
}

/// The latest exhausted reset of `resources` through the host's shared quota
/// record, which reads GitHub's rate-limit endpoint only when no process has
/// within the refresh interval. [`orbit_tools::github_quota::SharedReset::Unguarded`]
/// while the guard is off.
pub(in crate::executor::automation::vcs) fn shared_exhausted_reset(
    workspace: &Path,
    timeout_ms: u64,
    resources: &[&str],
) -> Result<orbit_tools::github_quota::SharedReset, OrbitError> {
    let request = build_vcs_request(
        "gh",
        vec!["api".into(), "rate_limit".into()],
        Some(workspace),
        timeout_ms,
    )?;
    Ok(orbit_tools::github_quota::shared_exhausted_reset(
        &request, resources,
    ))
}

pub(in crate::executor::automation::vcs) fn succeeded(
    result: orbit_exec::ExecutionResult,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    if !result.success {
        let message = format!(
            "private automation VCS {operation} failed: {}",
            result.stderr.trim()
        );
        return Err(OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
            operation: operation.to_string(),
            class: classify(&message),
            message,
            read_throttle: None,
        })));
    }
    Ok(result)
}

/// [`succeeded`] for a read through `gh`: GitHub's authenticated rate limit
/// keeps its class and message and gains the typed
/// [`orbit_common::ProviderReadThrottle`], so the caller's policy can wait
/// for the reset instead of failing the step [ORB-25864].
pub(in crate::executor::automation::vcs) fn succeeded_read(
    result: orbit_exec::ExecutionResult,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let throttle = if result.success {
        None
    } else {
        orbit_tools::github_cli::check_gh_read_result(&result, operation)
            .err()
            .and_then(|error| error.private_vcs_failure()?.read_throttle.clone())
    };
    succeeded(result, operation).map_err(|error| match (error, throttle) {
        (OrbitError::PrivateVcs(mut failure), Some(throttle)) => {
            failure.read_throttle = Some(throttle);
            OrbitError::PrivateVcs(failure)
        }
        (error, _) => error,
    })
}
