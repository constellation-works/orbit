//! Select the review findings a verification run judges, draw each one's
//! verifier crew and partition them by crew.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;
use crate::application::task::batch_prepare::{
    BatchSpec, Qualification, held_by_run_detail, prepare_batch,
};
use crate::application::task::{
    TaskListFilter, VERIFIABLE_STATUSES, finding_fingerprint, is_review_finding, needs_verification,
};

use crate::application::task::pilot::input::requested_workspace_root;
use crate::application::task::pilot::source::resolve_source_snapshot;

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = runtime.state_claim(input).map_err(failed)?;
    // Verdicts cite `path:line` at one revision, so there is nothing to
    // verify against outside a Git work tree.
    let source =
        resolve_source_snapshot(runtime, action, input, &workspace_root)?.ok_or_else(|| {
            DispatchError::action_failed(
                action,
                "finding verification pins a Git revision; the workspace is not a Git work tree",
            )
        })?;
    prepare_batch(
        runtime,
        action,
        input,
        run_id,
        BatchSpec {
            claim,
            source_revision: Some(source.source_revision.clone()),
            source: source.to_json(),
            workspace_path: workspace_root,
            automatic_filter: TaskListFilter {
                statuses: Some(VERIFIABLE_STATUSES.to_vec()),
                task_type: Some(TaskType::Bug),
                ..Default::default()
            },
            silent_reasons: &[],
            before_workspace: Vec::new(),
            before_task_count: vec![("round", json!("initial"))],
        },
        |runtime| runtime.finding_verification_holders(),
        is_review_finding,
        |task, held, explicit| {
            let history = runtime.get_task_history(&task.id)?;
            let reason = if !is_review_finding(task) {
                Some("not_a_review_finding")
            } else if !VERIFIABLE_STATUSES.contains(&task.status) {
                Some("status_not_verifiable")
            } else if held.contains_key(&task.id) {
                Some("already_verifying")
            } else if !explicit && !needs_verification(task, &history) {
                // An explicit selection re-verifies a confirmed finding on request.
                Some("already_confirmed")
            } else {
                None
            };
            Ok(match reason {
                Some(reason) => Qualification::Rejected(reason),
                None => Qualification::Selected(history.len()),
            })
        },
        |task, history_len| {
            let verifier = runtime.first_finding_verifier(task)?;
            let crew = verifier.crew.clone();
            Ok((task_snapshot(task, history_len, &verifier, &[]), crew))
        },
        skipped_detail,
        |tasks, task_ids| {
            let draws = tasks
                .iter()
                .filter(|task| task_ids.iter().any(|id| task["task_id"] == *id))
                .map(|task| json!({"task_id": task["task_id"], "eligible_pool": task["eligible_pool"]}))
                .collect::<Vec<_>>();
            Some(("draws", json!(draws)))
        },
    )
}

/// The prepared record of one task apply validates its verdict against.
pub(super) fn task_snapshot(
    task: &Task,
    history_len: usize,
    verifier: &crate::application::task::VerifierAssignment,
    prior: &[Value],
) -> Value {
    json!({
        "task_id": task.id,
        "title": task.title,
        "status": task.status,
        "history_len": history_len,
        "finding_fingerprint": finding_fingerprint(task),
        "crew": verifier.crew,
        "tier": verifier.tier,
        "eligible_pool": verifier.eligible_pool,
        "prior": prior,
    })
}

fn skipped_detail(reason: &str, holders: Option<&BTreeSet<String>>) -> String {
    match reason {
        "not_a_review_finding" => {
            "only a bug task filed by a review is verified; this task is not one".into()
        }
        "status_not_verifiable" => "only a proposed, someday or backlog finding is verified".into(),
        "already_verifying" => held_by_run_detail("verified", holders),
        _ => reason.into(),
    }
}
