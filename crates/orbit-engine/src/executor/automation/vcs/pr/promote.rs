use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, NO_DIFF_EXPECTED_TAG, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::input::{input_string_field, required_input_string};
use super::super::base_obsolescence::ensure_base_can_still_land;
use super::super::commit::already_landed::verify_handoff;
use super::super::freshness::commit_sha;
use super::super::handoff::{
    FailedHandoffPhase, HandoffContext, load_handoff_context, record_failed_handoff,
};
use super::attribution::pr_review_attribution;

pub(in crate::executor::automation) fn pr_promote<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let context = load_handoff_context(host, input, "pr_promote")?;
    let no_diff_expected = input
        .get("no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pr_number = if no_diff_expected {
        if let Some(checkpoint) = input.get("already_landed_checkpoint").filter(|value| {
            value.get("decision").and_then(Value::as_str) == Some("verified_already_landed")
        }) {
            verify_handoff(
                host,
                &context.tasks,
                &context.workspace_path,
                input_string_field(input, "run_id")
                    .as_deref()
                    .unwrap_or(&context.batch_id),
                checkpoint,
            )?;
        } else if context
            .tasks
            .iter()
            .any(|task| !task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG))
        {
            return Err(OrbitError::Execution(
                "pr_promote: no_diff_expected requires every task to carry the no-diff-expected tag"
                    .to_string(),
            ));
        }
        None
    } else {
        Some(required_input_string(input, "pr_number")?.to_string())
    };

    // ORB-10644: a resume can enter the pipeline here, with a PR opened before
    // its base went obsolete. Promotion is where this run first calls the work
    // delivered, so it re-asks the question rather than trusting the earlier
    // step's success.
    if !no_diff_expected && let Err(error) = ensure_promotable_base(input, &context) {
        record_failed_handoff(
            host,
            &context,
            input,
            FailedHandoffPhase::ObsoleteBase,
            &error,
        )?;
        return Err(error);
    }

    let mut performed_task_ids = Vec::new();
    let mut reused_task_ids = Vec::new();
    for task in &context.tasks {
        let has_pr = pr_number
            .as_deref()
            .is_none_or(|number| task.github_pr_number() == Some(number));
        if matches!(task.status, TaskStatus::Review | TaskStatus::Done) && has_pr {
            reused_task_ids.push(task.id.clone());
            continue;
        }

        let model = pr_review_attribution(host, task, &context.batch_id)?;
        let external_refs = match pr_number.as_deref() {
            Some(number) => vec![ExternalRef::github_pr(number.to_string())?],
            None => Vec::new(),
        };
        let update = TaskAutomationUpdate {
            status: (task.status != TaskStatus::Done).then_some(TaskStatus::Review),
            external_refs,
            model,
            ..TaskAutomationUpdate::default()
        };
        if let Err(error) = host.apply_task_automation_update(&task.id, update) {
            record_failed_handoff(host, &context, input, FailedHandoffPhase::Promote, &error)?;
            return Err(error);
        }
        performed_task_ids.push(task.id.clone());
    }

    let decision = if performed_task_ids.is_empty() {
        "reused"
    } else {
        "performed"
    };
    Ok(json!({
        "phase": "promote",
        "decision": decision,
        "performed_task_ids": performed_task_ids,
        "reused_task_ids": reused_task_ids,
        "pr_number": pr_number,
        "pr_url": input_string_field(input, "pr_url"),
        "no_diff_expected": no_diff_expected,
    }))
}

/// Re-run the base-obsolescence gate for a promotion.
///
/// The promote step is handed the base *name* and the moving `base_ref`, not
/// the run's pinned checkpoint, so the base commit is resolved from whichever
/// the caller supplied. A caller that names no base (the no-diff promotion, and
/// the direct-promotion tests) has no delivery target to check.
fn ensure_promotable_base(input: &Value, context: &HandoffContext) -> Result<(), OrbitError> {
    let Some(base) = input_string_field(input, "base") else {
        return Ok(());
    };
    let base_sha = match input_string_field(input, "base_sha") {
        Some(base_sha) => base_sha,
        None => {
            let base_ref = input_string_field(input, "base_ref").unwrap_or_else(|| base.clone());
            commit_sha(&context.workspace_path, &base_ref)?
        }
    };
    ensure_base_can_still_land(
        &context.workspace_path,
        "pr_promote",
        &base,
        &base_sha,
        input,
    )
}
