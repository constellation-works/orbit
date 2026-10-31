//! Read-only "Why stuck" facts for a blocked task or a held delivery run.
//!
//! The card is a detail-only sibling of `delivery_attention`. List rows keep
//! the attention badge they already have, and this module does not change
//! when that badge is emitted. Forge errors stay on the card as an explicit
//! unavailable row; they do not fail the task page.

use orbit_core::application::job::delivery_attention::{
    LANDING_REVIEW_SETTLE_STEP, landing_review_gate_facts,
};
use orbit_core::application::job::run_display_steps;
use orbit_core::application::task::TaskRow;
use orbit_core::{JobRun, JobRunState, OrbitError, OrbitRuntime, Task, TaskStatus};
use serde_json::{Map, Value, json};

use crate::api::failure_root_line;

/// Attach `why_stuck` when this task is blocked or its latest delivery run is held.
///
/// Other tasks are left unchanged, including their forge: the pull-request
/// read runs only for a card the page will show.
pub(crate) fn attach_why_stuck(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    detail: &mut Map<String, Value>,
) -> Result<(), OrbitError> {
    let run = runtime.newest_single_task_delivery_run(&row.task.id)?;
    if row.task.status != TaskStatus::Blocked && !held(&run) {
        return Ok(());
    }
    let landing = landing_review(runtime, run.as_ref());
    detail.insert(
        "why_stuck".to_string(),
        json!({
            "escalation_note": escalation_note(detail),
            "landing_review": landing,
            "root_cause": root_cause(runtime, run.as_ref(), &landing),
            "pull_request": pull_request(runtime, &row.task),
        }),
    );
    Ok(())
}

fn held(run: &Option<JobRun>) -> bool {
    run.as_ref()
        .is_some_and(|run| run.state == JobRunState::Held)
}

fn escalation_note(detail: &Map<String, Value>) -> Value {
    let block = detail.get("block").and_then(Value::as_object);
    let cause = block
        .and_then(|block| block.get("cause"))
        .and_then(Value::as_str);
    let reason = block
        .and_then(|block| block.get("reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if cause == Some("escalation") {
        return match reason {
            Some(text) => json!({ "availability": "read", "text": text }),
            None => json!({ "availability": "unavailable" }),
        };
    }
    json!({ "availability": "none" })
}

fn landing_review(runtime: &OrbitRuntime, run: Option<&JobRun>) -> Value {
    let Some(run) = run else {
        return json!({ "availability": "unavailable" });
    };
    let state = match runtime.read_run_state(&run.run_id) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                run_id = %run.run_id,
                %error,
                "why stuck continued without the landing-review pipeline state"
            );
            None
        }
    };
    let output = state
        .as_ref()
        .and_then(|state| state.pipeline_step_output(LANDING_REVIEW_SETTLE_STEP));
    let display = run_display_steps(run, || match runtime.collect_run_audit_steps(&run.run_id) {
        Ok(steps) => steps,
        Err(error) => {
            tracing::warn!(
                run_id = %run.run_id,
                %error,
                "why stuck continued without landing-review audit steps"
            );
            Vec::new()
        }
    });
    let step_error = display
        .records
        .iter()
        .rev()
        .find(|step| step.target_id == LANDING_REVIEW_SETTLE_STEP)
        .and_then(|step| step.error_message.as_deref());
    landing_review_gate_facts(output, step_error)
}

fn root_cause(runtime: &OrbitRuntime, run: Option<&JobRun>, landing: &Value) -> Value {
    let Some(run) = run else {
        return json!({ "availability": "unavailable" });
    };
    if let Some(text) = failure_root_line(runtime, run) {
        return json!({ "availability": "read", "text": text });
    }
    let detail = match landing.get("verdict").and_then(Value::as_str) {
        Some("awaiting evidence") => Some("awaiting evidence"),
        Some("forge unavailable") => Some("forge unavailable"),
        _ => None,
    };
    match detail {
        Some(detail) => json!({
            "availability": "read",
            "text": format!(
                "{} {} at {LANDING_REVIEW_SETTLE_STEP} - {detail}",
                run.run_id,
                run.state
            ),
        }),
        None => json!({ "availability": "unavailable" }),
    }
}

fn pull_request(runtime: &OrbitRuntime, task: &Task) -> Value {
    let Some(number) = task
        .github_pr_number()
        .and_then(|raw| raw.parse::<u64>().ok())
    else {
        return unavailable_pull_request("no pull request");
    };
    match runtime
        .task_pr_forge()
        .pull_request_blockers(&runtime.paths().repo_root, number)
    {
        Ok(Some(blockers)) => {
            let mut checks = Vec::new();
            for (state, names) in [
                ("failed", blockers.failed_required_checks.as_slice()),
                ("passed", blockers.passed_required_checks.as_slice()),
                ("pending", blockers.pending_required_checks.as_slice()),
            ] {
                for name in names {
                    checks.push(json!({ "name": name, "state": state }));
                }
            }
            json!({
                "availability": "read",
                "mergeable": if blockers.conflicting { "conflicting" } else { "mergeable" },
                "checks": checks,
            })
        }
        Ok(None) => unavailable_pull_request("no open pull request"),
        Err(OrbitError::PullRequestMergeabilityUnknown { .. }) => {
            unavailable_pull_request("mergeability still calculating")
        }
        Err(error) => {
            tracing::warn!(
                pr = number,
                %error,
                "why stuck could not read pull request blockers"
            );
            unavailable_pull_request("forge unavailable")
        }
    }
}

fn unavailable_pull_request(detail: &str) -> Value {
    json!({
        "availability": "unavailable",
        "detail": detail,
        "mergeable": Value::Null,
        "checks": [],
    })
}
