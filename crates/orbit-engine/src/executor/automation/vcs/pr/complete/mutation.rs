//! Observation failures, mutation records, merge requests, capabilities and PR number resolution.

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use orbit_types::workflow::{
    PR_COMPLETION_MUTATION_STARTED_EVENT, pr_completion_has_mutation_provenance,
};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::handoff::load_handoff_context;
use super::super::super::operations;
use super::super::answers::ReviewedMergeOutcome;
use super::super::merge::{MergeStrategy, PermittedMerge, resolve_permitted_merge};

/// [ORB-16307] A failed authenticated read made before any completion mutation:
/// the PR status, required-check and merge-policy reads, and the base-refresh
/// and infrastructure prerequisite reads. Only a transport failure within the
/// cumulative budget, with neither a mutation in this invocation nor durable
/// repair or mutation provenance, becomes `completion_observation_unavailable`,
/// which the host may resume only after authenticating the unchanged prefix.
/// The transport class is the one the provider boundary decided; the result
/// carries [`PrivateVcsFailureClass::CompletionObservationUnavailable`] beside
/// its unchanged message. Every other failure keeps its own error and
/// recovery path.
pub(super) fn observation_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    mutated: bool,
    deadline_ms: Option<u64>,
    error: OrbitError,
) -> OrbitError {
    let within_budget = deadline_ms.is_none_or(|deadline| {
        super::super::super::completion_deadline::now_ms().is_ok_and(|now| now < deadline)
    });
    match error.private_vcs_failure() {
        Some(failure)
            if !mutated
                && within_budget
                && failure.class == PrivateVcsFailureClass::TransientObservation
                && completion_history_is_read_only(host, input).unwrap_or(false) =>
        {
            OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
                operation: failure.operation.clone(),
                class: PrivateVcsFailureClass::CompletionObservationUnavailable,
                message: format!("completion_observation_unavailable: {error}"),
            }))
        }
        _ => error,
    }
}

/// Read reservations after pre-observation repair adoption/publication as well
/// as after polling. Absence is only eligibility: the host still authenticates
/// the exact original publication, native receipts and current policy.
fn completion_history_is_read_only<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<bool, OrbitError> {
    let context = load_handoff_context(host, input, "pr_complete")?;
    for task in context.tasks {
        if pr_completion_has_mutation_provenance(&host.get_task_history(&task.id)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Persist before entering a mutation boundary. In particular an unknown merge
/// reply must remain disqualified when a later invocation's local flags reset.
pub(super) fn record_completion_mutation<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    operation: &str,
) -> Result<(), OrbitError> {
    let context = load_handoff_context(host, input, "pr_complete")?;
    for task in context.tasks {
        host.apply_task_automation_update(
            &task.id,
            crate::context::TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".into(),
                    event: PR_COMPLETION_MUTATION_STARTED_EVENT.into(),
                    note: Some(
                        json!({"run_id":context.batch_id,"operation":operation,
                        "pr_number":input["pr_number"],"head_sha":input["published_head_sha"]})
                        .to_string(),
                    ),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

pub(super) fn request_merge<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    strategy: MergeStrategy,
    auto: bool,
    reviewed_head_sha: Option<&str>,
) -> Result<ReviewedMergeOutcome, OrbitError> {
    record_completion_mutation(host, input, if auto { "auto_merge" } else { "merge" })?;
    let response = host.run_private_vcs_operation(
        operations::PR_MERGE,
        json!({
            "pr": pr_number,
            "strategy": strategy.as_str(),
            "auto": auto,
            "reviewed_head_sha": reviewed_head_sha,
            "report_base_modified": reviewed_head_sha.is_some(),
            "workspace_path": workspace_path,
        }),
    )?;
    Ok(ReviewedMergeOutcome::from_response(&response))
}

pub(super) fn resolved_capabilities<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    selected: &mut Option<PermittedMerge>,
) -> Result<PermittedMerge, OrbitError> {
    if let Some(capabilities) = selected {
        return Ok(capabilities.clone());
    }
    let capabilities =
        resolve_permitted_merge(host, workspace_path, pr_number).map_err(|error| {
            let wrapper = OrbitError::Execution(format!(
                "pr_complete: could not resolve a permitted merge method for pull request \
             #{pr_number}: {error}; the task stays in review"
            ));
            super::super::super::operations::carry_class(&error, wrapper)
        })?;
    *selected = Some(capabilities.clone());
    Ok(capabilities)
}

pub(super) fn resolve_pr_number(input: &Value, tasks: &[Task]) -> Result<String, OrbitError> {
    if let Some(pr_number) = input_string_field(input, "pr_number") {
        return Ok(pr_number);
    }
    tasks
        .iter()
        .find_map(Task::github_pr_number)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "pr_complete: no pr_number supplied and no task in the batch carries a github-pr \
                 external ref"
                    .to_string(),
            )
        })
}

pub(super) fn ensure_all_tasks_no_diff_expected(tasks: &[Task]) -> Result<(), OrbitError> {
    if tasks
        .iter()
        .any(|task| !task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG))
    {
        return Err(OrbitError::Execution(
            "pr_complete: no_diff_expected requires every task to carry the no-diff-expected tag"
                .to_string(),
        ));
    }
    Ok(())
}
