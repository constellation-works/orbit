//! Every pull request merge Orbit requests leaves an audit row naming its run
//! and step [ORB-26389].
//!
//! A delivery pull request merged by the owner sweep left no audit row, so
//! nothing said which process merged it or on whose authority. Each merge
//! site now goes through here: the owner sweep's automatic landing, the
//! `orbit.workflow.land` call, an operator's handoff landing decision and the
//! engine's private `pr.merge` operation (completion, batch merge and the
//! owner's handoff landing job).
//!
//! A merge writes two rows under the `pr_merge` command: `requested` before
//! the forge is asked, and `merged`, `auto_merge_enabled`, `refused`,
//! `ambiguous` or `failed` after. The
//! `requested` row is written first and fails closed: when it cannot be
//! written the merge is not requested, so no merge goes unaudited. The
//! outcome row is best effort, because the merge has already happened or not
//! by then, and failing the caller would misreport it.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::MergeMessage;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::task_pr_forge::{PullRequestMergeOutcome, TaskPrForge};

/// The audit command every Orbit-initiated pull request merge records.
const PR_MERGE_AUDIT_COMMAND: &str = "pr_merge";

/// Who asked for a merge: the run whose authority it acts on and the step
/// (or owner pass) that requested it.
pub(crate) struct PullRequestMergeAttribution<'a> {
    /// The run the merge is attributed to; `None` falls back to the executing
    /// activity's `ORBIT_RUN_ID`, when there is one.
    pub(crate) run_id: Option<&'a str>,
    /// The step or owner pass that requested the merge.
    pub(crate) step: &'a str,
    /// The task the merge delivers, when one is known.
    pub(crate) task_id: Option<&'a str>,
}

/// Merge `number` at `head_sha` through `forge`, auditing the request and its
/// outcome.
pub(crate) fn audited_forge_merge(
    runtime: &OrbitRuntime,
    forge: &dyn TaskPrForge,
    repo_root: &Path,
    number: u64,
    head_sha: &str,
    message: &MergeMessage,
    attribution: &PullRequestMergeAttribution<'_>,
) -> Result<PullRequestMergeOutcome, OrbitError> {
    let pull_request = number.to_string();
    let request = json!({"pull_request": number, "head_sha": head_sha});
    record_merge_audit(
        runtime,
        attribution,
        &pull_request,
        "requested",
        AuditEventStatus::Success,
        request.clone(),
    )?;
    let outcome = forge.merge_pull_request(repo_root, number, head_sha, message);
    let (phase, status, detail) = match &outcome {
        Ok(PullRequestMergeOutcome::Merged { merge_commit }) => (
            "merged",
            AuditEventStatus::Success,
            json!({"merge_commit": merge_commit}),
        ),
        Ok(PullRequestMergeOutcome::Refused { reason }) => (
            "refused",
            AuditEventStatus::Denied,
            json!({"reason": reason}),
        ),
        Err(error) => (
            "failed",
            AuditEventStatus::Failure,
            json!({"error": error.to_string()}),
        ),
    };
    record_merge_outcome(
        runtime,
        attribution,
        &pull_request,
        phase,
        status,
        merged_fields(request, detail),
    );
    outcome
}

/// Run the engine's private `pr.merge` operation, auditing the request and its
/// outcome. The engine names its run and step in
/// [`orbit_engine::PR_MERGE_ATTRIBUTION_FIELD`].
pub(crate) fn audited_private_vcs_merge(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    let named = |field: &str| {
        input
            .get(orbit_engine::PR_MERGE_ATTRIBUTION_FIELD)
            .and_then(|attribution| attribution.get(field))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let attribution = PullRequestMergeAttribution {
        run_id: named("run_id"),
        step: named("step_id").unwrap_or("unnamed_step"),
        task_id: named("task_id"),
    };
    let pull_request = match input.get("pr") {
        Some(Value::String(selector)) => selector.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    };
    let request = json!({
        "pull_request": pull_request,
        "head_sha": input.get("reviewed_head_sha"),
        "strategy": input.get("strategy"),
        "auto": input.get("auto"),
    });
    record_merge_audit(
        runtime,
        &attribution,
        &pull_request,
        "requested",
        AuditEventStatus::Success,
        request.clone(),
    )?;
    let outcome =
        orbit_engine::run_private_vcs_operation(orbit_engine::PRIVATE_VCS_PR_MERGE, input);
    let (phase, status, detail) = match &outcome {
        Ok(response) => {
            let (phase, status) = if response.get("ambiguous_merge").is_some() {
                // The forge's reply was lost: the merge may or may not exist.
                ("ambiguous", AuditEventStatus::Failure)
            } else if response.get("merged").and_then(Value::as_bool) == Some(false) {
                // A synchronous refusal is an answer, not a merge.
                ("refused", AuditEventStatus::Denied)
            } else if input.get("auto").and_then(Value::as_bool) == Some(true) {
                // The forge merges later, once its own checks pass.
                ("auto_merge_enabled", AuditEventStatus::Success)
            } else {
                ("merged", AuditEventStatus::Success)
            };
            (
                phase,
                status,
                json!({
                    "merge_commit": response.get("landed_commit"),
                    "refusal": response.get("refusal"),
                    "ambiguous_merge": response.get("ambiguous_merge"),
                }),
            )
        }
        Err(error) => (
            "failed",
            AuditEventStatus::Failure,
            json!({"error": error.to_string()}),
        ),
    };
    record_merge_outcome(
        runtime,
        &attribution,
        &pull_request,
        phase,
        status,
        merged_fields(request, detail),
    );
    outcome
}

fn merged_fields(mut request: Value, detail: Value) -> Value {
    if let (Value::Object(request), Value::Object(detail)) = (&mut request, detail) {
        request.extend(detail);
    }
    request
}

fn record_merge_outcome(
    runtime: &OrbitRuntime,
    attribution: &PullRequestMergeAttribution<'_>,
    pull_request: &str,
    phase: &str,
    status: AuditEventStatus,
    detail: Value,
) {
    if let Err(error) =
        record_merge_audit(runtime, attribution, pull_request, phase, status, detail)
    {
        tracing::warn!(
            target: "orbit.core.external_merge",
            pull_request,
            phase,
            step = attribution.step,
            %error,
            "could not record the pull request merge outcome audit row"
        );
    }
}

fn record_merge_audit(
    runtime: &OrbitRuntime,
    attribution: &PullRequestMergeAttribution<'_>,
    pull_request: &str,
    phase: &str,
    status: AuditEventStatus,
    detail: Value,
) -> Result<(), OrbitError> {
    let mut payload = json!({
        "run_id": attribution.run_id,
        "step": attribution.step,
        "task_id": attribution.task_id,
    });
    if let (Value::Object(payload), Value::Object(detail)) = (&mut payload, detail) {
        payload.extend(detail);
    }
    let workspace_id = runtime.workspace_id().ok();
    record_system_audit_event(
        runtime,
        SystemAuditEvent {
            execution_id_prefix: "audit-pr-merge",
            command: PR_MERGE_AUDIT_COMMAND,
            subcommand: Some(phase),
            tool_name: None,
            target_type: "pull_request",
            target_id: Some(pull_request).filter(|number| !number.is_empty()),
            role: "system",
            status,
            arguments_json: payload.to_string(),
            workspace_id: workspace_id.as_deref(),
            task_id: attribution.task_id,
            attribution: match attribution.run_id {
                Some(run_id) => AuditRunAttribution::Run(Some(run_id.to_string())),
                None => AuditRunAttribution::Environment { run_id: None },
            },
        },
    )
}
