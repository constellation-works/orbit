//! Completion routing for a verified workflow failure with no check jobs.
use orbit_common::OrbitError;
use orbit_types::workflow::{
    PR_CANDIDATE_WORKFLOW_FAILED_EVENT, PR_CANDIDATE_WORKFLOW_FAILURE_MARKER, PrAutoFixBlocker,
    PrInfrastructureEvidence,
};
use serde_json::json;

use super::auto_fix::{self, AutoFix};
use crate::context::RuntimeHost;

pub(super) fn failed_workflow_diagnosis(evidence: &PrInfrastructureEvidence) -> String {
    let failure = evidence.failed_workflow_run();
    format!(
        "{} workflow {} run {} (attempt {}) on head {} completed with conclusion {} without creating any jobs; required check '{}' was not reported. {}",
        if failure.is_some_and(|failure| failure.candidate_owned) {
            PR_CANDIDATE_WORKFLOW_FAILURE_MARKER
        } else {
            "infrastructure_workflow_failure:"
        },
        evidence.workflow_path,
        evidence.run_id,
        evidence.run_attempt,
        evidence.head_sha,
        evidence.run["conclusion"].as_str().unwrap_or("unavailable"),
        evidence.missing.context,
        failure
            .and_then(|failure| failure.provider_message.as_deref())
            .unwrap_or("The provider supplied no additional diagnosis.")
    )
}

/// Admit the ordinary failed-check repair directly, retaining the workflow
/// evidence before the reservation. No classification rerun precedes it.
pub(super) fn repair_failed_workflow<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr: &str,
    evidence: &PrInfrastructureEvidence,
) -> OrbitError {
    let diagnosis = failed_workflow_diagnosis(evidence);
    let retained = auto_fix::record_base_path(
        host,
        auto_fix,
        PR_CANDIDATE_WORKFLOW_FAILED_EVENT,
        &json!(evidence),
    )
    .and_then(|()| {
        auto_fix::attach_evidence(
            host,
            auto_fix,
            pr,
            "failed workflow evidence has no owning run",
            &format!(
                "auto-fix/{}/workflow-{}-failed.json",
                auto_fix.run_id, evidence.run_id
            ),
            &json!(evidence),
        )
    });
    if let Err(error) = retained {
        return auto_fix::stopped(
            pr,
            &format!("{diagnosis}; its evidence could not be retained: {error}"),
        );
    }
    match auto_fix::admit(
        host,
        auto_fix,
        pr,
        &evidence.head_sha,
        PrAutoFixBlocker::FailedChecks,
    ) {
        Ok(attempt) => auto_fix::check_repair_handoff(pr, &evidence.head_sha, &attempt, &diagnosis),
        Err(OrbitError::Execution(message)) => {
            OrbitError::Execution(format!("{message}\n{diagnosis}"))
        }
        Err(OrbitError::DeliveryRefused { refusal, message }) => OrbitError::DeliveryRefused {
            refusal,
            message: format!("{message}\n{diagnosis}"),
        },
        Err(error) => error,
    }
}
