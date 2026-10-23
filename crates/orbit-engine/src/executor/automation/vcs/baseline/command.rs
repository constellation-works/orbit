use super::super::required::command::{
    RequiredCommandRun, VALIDATION_TIMEOUT_MS, run_required_command,
};
use crate::context::RuntimeHost;
use orbit_common::OrbitError;
use regex::Regex;
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Reruns of a network-inconclusive failure, after the first attempt.
pub(in super::super) const NETWORK_RETRIES: u32 = 2;

/// Wait before each rerun of a network-inconclusive failure.
const NETWORK_RETRY_BACKOFF: [Duration; NETWORK_RETRIES as usize] =
    [Duration::from_secs(2), Duration::from_secs(6)];

/// Evidence that a failed command could not reach the network. Landing reads
/// a failed hosted check's log with the same rule [ORB-15956].
pub(in super::super) fn network_inconclusive(output: &str) -> Option<String> {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    let pattern = PATTERN
        .get_or_init(|| {
            Regex::new(
                r"(?im)^.*(?:\bstatus 000\b|\bhttp(?:_code)?[ :=]+000\b|\bcurl: \((?:5|6|7|28|35|56)\)|could not resolve (?:host|proxy)|temporary failure in name resolution|name or service not known|\beai_again\b|getaddrinfo .*(?:failed|enotfound)|tls handshake timeout|ssl connection timeout|handshake (?:timed out|operation timed out)|tls: handshake timeout).*$",
            )
            .ok()
        })
        .as_ref()?;
    pattern
        .find(output)
        .map(|line| line.as_str().trim().to_string())
}

/// Run one required command, rerunning a network-inconclusive failure with
/// backoff. The returned run records how many reruns it took. `selection`,
/// from a candidate run's summary, is what a base run must test.
pub(in super::super) fn run_validation_command<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
    selection: Option<&Value>,
) -> Result<RequiredCommandRun, OrbitError> {
    run_validation_command_until(host, workspace_path, command, selection, None)
}

/// [`run_validation_command`], ending by `deadline` when one is set: each
/// attempt's timeout is shortened to what is left, and no rerun starts after
/// it.
pub(in super::super) fn run_validation_command_until<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    command: &str,
    selection: Option<&Value>,
    deadline: Option<Instant>,
) -> Result<RequiredCommandRun, OrbitError> {
    let started = Instant::now();
    let mut backoff_ms = 0_u64;
    let mut run = run_required_command(
        host,
        workspace_path,
        command,
        selection,
        attempt_timeout_ms(deadline),
    )?;
    for backoff in NETWORK_RETRY_BACKOFF {
        if run.passed
            || run.timed_out
            || run.missing_tool.is_some()
            || run.infrastructure_failure.is_some()
        {
            break;
        }
        let Some(evidence) = network_inconclusive(&run.output) else {
            break;
        };
        if deadline.is_some_and(|deadline| Instant::now() + backoff >= deadline) {
            break;
        }
        tracing::info!(
            command = run.command,
            evidence,
            retry = run.network_retries + 1,
            "required validation was network-inconclusive; retrying"
        );
        let wait_started = Instant::now();
        super::super::remote_probe::fixed_wait(backoff);
        backoff_ms = backoff_ms
            .saturating_add(u64::try_from(wait_started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let retries = run.network_retries + 1;
        let mut attempts = run.timing["attempts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        run = run_required_command(
            host,
            workspace_path,
            command,
            selection,
            attempt_timeout_ms(deadline),
        )?;
        if let Some(new) = run.timing["attempts"].as_array() {
            attempts.extend(new.iter().cloned());
        }
        run.timing["attempts"] = serde_json::json!(attempts);
        run.network_retries = retries;
    }
    if !run.passed
        && !run.timed_out
        && run.missing_tool.is_none()
        && run.infrastructure_failure.is_none()
    {
        run.network_evidence = network_inconclusive(&run.output);
    }
    run.timing["elapsed_ms"] = serde_json::json!(started.elapsed().as_millis());
    run.timing["network_retry_backoff_ms"] = serde_json::json!(backoff_ms);
    Ok(run)
}

/// One attempt's timeout: the validation ceiling, shortened to what is left
/// before `deadline`, and never zero.
fn attempt_timeout_ms(deadline: Option<Instant>) -> u64 {
    let left = deadline.map_or(u64::MAX, |deadline| {
        u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    });
    VALIDATION_TIMEOUT_MS.min(left).max(1)
}
