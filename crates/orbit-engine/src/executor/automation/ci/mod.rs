//! `collect_ci_evidence` — host-owned CI discovery.
//!
//! An implementation agent cannot query GitHub from inside its sandbox: the
//! `github.*` builtins run `gh` as a child of the executing process, and the
//! lane denies the CLI's credential directory and forwards no token. The
//! problem disappears entirely if the host does the looking *before* any task
//! exists, and writes what it found into the task description.
//!
//! This stage sits on the same engine-private automation boundary as
//! `automation::vcs`: `NoSandbox`, inherited environment, engine-private
//! labels, never advertised to agents, and outside public tool authorization
//! and activity allowlists. No new agent-facing tool is added and no GitHub
//! credential becomes visible from inside a sandbox. What leaves this module
//! is one bounded, redacted JSON snapshot — no token, no host configuration,
//! and no way to run another query.
//!
//! Four endings stay distinct here and are never allowed to collapse into one
//! another or into a clean pass: [`OUTCOME_CAPABILITY_UNAVAILABLE`] (we could
//! not look), [`OUTCOME_NO_CURRENT_FAILURE`] (we looked and nothing is
//! failing), [`OUTCOME_CURRENT_FAILURES`] (we looked and something is), and
//! [`OUTCOME_RETRYABLE_ERROR`] (a bounded discovery or investigation failed).

mod collect;
mod investigate;
mod query;

pub(in crate::executor::automation) use query::AuthStatus;

use std::path::PathBuf;

use orbit_common::OrbitError;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::input::{canonicalize_existing_dir, input_string_field};

/// A GitHub client was absent or unauthenticated on this host, so no CI
/// evidence was gathered. Never a clean bill of health.
pub(super) const OUTCOME_CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
/// The queries ran and found no current, non-superseded failure.
pub(super) const OUTCOME_NO_CURRENT_FAILURE: &str = "no_current_failure";
/// The queries ran and found at least one current failure to file.
pub(super) const OUTCOME_CURRENT_FAILURES: &str = "current_failures";
/// A discovery or investigation failed. The partial evidence remains visible,
/// but the pipeline must retry rather than consuming it as clean.
pub(super) const OUTCOME_RETRYABLE_ERROR: &str = "retryable_error";

/// Run conclusions that are not a pass.
///
/// `cancelled` and `timed_out` are in here deliberately: a run that never
/// produced a verdict is not a green run, and treating it as one is exactly
/// how a red pipeline gets reported as clean. Job expansion later separates
/// a cancelled job with no failed steps (inconclusive, nothing to repair)
/// from a cancelled or mixed run that still has failed steps.
pub(super) fn unsuccessful_conclusion(conclusion: Option<&str>) -> bool {
    matches!(
        conclusion,
        Some("failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure")
    )
}

pub(super) fn optional_input_string(input: &Value, key: &str) -> Option<String> {
    input_string_field(input, key)
}

/// Read an optional bound, clamped to `max`.
pub(super) fn bounded_u64(
    input: &Value,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        OrbitError::InvalidInput(format!("input.{key} must be a non-negative integer"))
    })?;
    Ok(raw.min(max))
}

/// The checkout this stage queries from: an explicit `workspace_path` when the
/// caller supplied one, otherwise the workspace repository root. Both resolve
/// to the same remote.
fn query_root<H: RuntimeHost + ?Sized>(host: &H, input: &Value) -> Result<PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path"),
        None => Ok(PathBuf::from(host.repo_root()?)),
    }
}

pub(super) fn collect_ci_evidence<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let queries = query::HostCiQueries::new(&query_root(host, input)?);
    let evidence = collect::collect(&queries, input)?;
    Ok(serde_json::json!({
        "phase": "collect_ci_evidence",
        "ci_evidence": evidence,
    }))
}

#[cfg(test)]
mod tests;
