//! One bounded, isolated candidate replay after a comparable passing base.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{FlakyValidation, MAX_FLAKY_TESTS};

use crate::context::RuntimeHost;

use super::base_claim::failing_tests_selection;
use super::baseline::BaselineCheck;
use super::required_command::{RequiredCommandRun, VALIDATION_TIMEOUT_MS, run_required_command};

/// An attempted replay is retained even when it fails or cannot prove coverage.
pub(super) struct CandidateRerun {
    pub(super) run: RequiredCommandRun,
    pub(super) flaky: Option<FlakyValidation>,
}

pub(super) fn rerun_failed_tests<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    candidate: &str,
    run_id: &str,
    original: &RequiredCommandRun,
    baseline: Option<&BaselineCheck>,
) -> Result<Option<CandidateRerun>, OrbitError> {
    let Some(check) = baseline.filter(|check| check.decision() == "passed") else {
        return Ok(None);
    };
    if check
        .result
        .as_ref()
        .is_ok_and(|base| base.host_coverage_refused)
        || original.passed
        || original.timed_out
        || original.missing_tool.is_some()
        || original.infrastructure_failure.is_some()
        || original.network_evidence.is_some()
        || original.deferred_notice.is_some()
        || original.host_output_refusal.is_some()
    {
        return Ok(None);
    }
    let Some(selection) = failing_tests_selection(original.selection(), &original.failures, &[])
    else {
        return Ok(None);
    };
    let count = selection["failing_tests"].as_array().map_or(0, Vec::len);
    if count == 0 || count > MAX_FLAKY_TESTS {
        return Ok(None);
    }
    // NoSandbox is the shared required-command runner's host execution path.
    // A failed-test selection makes the affected-test gate run serially.
    let rerun = run_required_command(
        host,
        workspace,
        &original.command,
        Some(&selection),
        VALIDATION_TIMEOUT_MS,
    )?;
    let executed = rerun
        .summary
        .as_ref()
        .and_then(|summary| summary.tests_run)
        .unwrap_or(0);
    let accepted = rerun.passed
        && !rerun.timed_out
        && rerun.missing_tool.is_none()
        && rerun.infrastructure_failure.is_none()
        && rerun.deferred_notice.is_none()
        && rerun.host_output_refusal.is_none()
        && rerun.selection() == Some(&selection)
        && executed >= count as u64;
    let flaky = if accepted {
        let names = selection["failing_tests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(|test| format!("test {test}"))
            .collect();
        Some(FlakyValidation {
            command: original.command.clone(),
            base_sha: check.base_sha.clone(),
            tested_head: candidate.to_string(),
            run_id: run_id.to_string(),
            failures: names,
            selection,
            original_exit_code: original.exit_code,
            original_output: original.output.clone(),
            base_output: check
                .result
                .as_ref()
                .map(|base| base.output.clone())
                .unwrap_or_default(),
            rerun_output: rerun.output.clone(),
            rerun_tests_run: executed,
            validation_env: rerun.environment_record(),
        })
    } else {
        None
    };
    Ok(Some(CandidateRerun { run: rerun, flaky }))
}
