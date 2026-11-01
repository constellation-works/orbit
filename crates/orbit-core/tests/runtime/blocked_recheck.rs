//! The deterministic blocked-task recheck through its public boundary: a
//! blocked task whose delivery pull request merged outside Orbit settles to
//! `done` without an agent, the recheck records why, and a task whose pull
//! request is still unmerged is left blocked for final recovery. A task whose
//! required checks pass again requeues only when a base-red failure of the
//! pull request's current head caused its current block.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::recovery_fixture::{Fixture, fixture};
use orbit_core::OrbitError;
use orbit_core::application::task::RecheckKind;
use orbit_core::runtime::task_pr_forge::{
    ForgePullRequest, MergedPullRequest, PullRequestBlockers, TaskPrForge,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
use orbit_types::task::TaskHistoryEntry;
use serde_json::json;

/// A forge whose only pull request, #17, is merged (or not) as configured.
struct MergeForge {
    merged: Mutex<Option<MergedPullRequest>>,
}

impl TaskPrForge for MergeForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }
    fn merged_pull_requests(&self, _: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.merged.lock().unwrap().iter().cloned().collect())
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

fn merged_delivery(task: &str, base: &str) -> MergedPullRequest {
    MergedPullRequest {
        pull: ForgePullRequest {
            number: 17,
            head_branch: format!("orbit/{task}-0123abcd"),
            body: format!("Delivers {task}."),
            author: "orbit-bot".into(),
        },
        base_branch: base.into(),
        head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        merge_commit: "89abcdef0123456789abcdef0123456789abcdef".into(),
    }
}

#[test]
fn a_blocked_task_whose_pull_request_merged_externally_settles_without_an_agent() {
    let mut fx = fixture();
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    fx.escalate(&task);

    // Still unmerged: the recheck leaves the task blocked for final recovery.
    let forge = Arc::new(MergeForge {
        merged: Mutex::new(None),
    });
    fx.runtime = fx.runtime.clone().with_task_pr_forge(forge.clone());
    assert!(fx.runtime.recheck_blocked_tasks().unwrap().is_empty());
    assert_eq!(fx.status(&task), "blocked");

    // The operator merges the PR outside Orbit.
    *forge.merged.lock().unwrap() = Some(merged_delivery(&task, "main"));
    let settled = fx.runtime.recheck_blocked_tasks().unwrap();
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0].task_id, task);
    assert_eq!(settled[0].kind, RecheckKind::ExternalMerge);
    assert!(
        settled[0].why.contains("#17") && settled[0].why.contains("89abcdef"),
        "{}",
        settled[0].why
    );
    assert_eq!(fx.status(&task), "done");

    // The settlement records why in history, once.
    let history = fx.runtime.get_task_history(&task).unwrap();
    let rechecked = history
        .iter()
        .filter(|entry| entry.event == "blocked_task_rechecked")
        .collect::<Vec<_>>();
    assert_eq!(rechecked.len(), 1, "{history:?}");
    let note = rechecked[0].note.as_deref().unwrap();
    assert!(
        note.starts_with("kind=external_merge why=") && note.contains("#17"),
        "{note}"
    );

    // Nothing is left to settle.
    assert!(fx.runtime.recheck_blocked_tasks().unwrap().is_empty());
}

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// A forge whose only pull request, #17, is open at `head` with its required
/// check `CI` passing.
struct GreenForge {
    head: String,
}

impl TaskPrForge for GreenForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }
    fn pull_request_blockers(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        Ok((number == 17).then(|| PullRequestBlockers {
            pull: ForgePullRequest {
                number,
                head_branch: "orbit/fixture".into(),
                body: String::new(),
                author: "orbit-bot".into(),
            },
            head_sha: self.head.clone(),
            conflicting: false,
            failed_required_checks: Vec::new(),
            pending_required_checks: Vec::new(),
            passed_required_checks: vec!["CI".into()],
        }))
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

/// Record `head`'s failure as inherited from the base, as the auto-fix poll
/// does while a run is watching the pull request.
fn record_base_red(fx: &Fixture, task: &str, head: &str) {
    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        task,
        TaskAutomationUpdate {
            append_history: vec![TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: "system".into(),
                event: "pr_auto_fix_base_red".into(),
                note: Some(json!({"failed_head": head, "reason": "base_red"}).to_string()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
}

/// A run of `task` that blocks it as a failed run does, after recording a
/// base-red failure for `base_red_head` when given.
fn run_blocks(fx: &Fixture, task: &str, base_red_head: Option<&str>) {
    fx.update(json!({"id": task, "status": "in_progress"}));
    if let Some(head) = base_red_head {
        record_base_red(fx, task, head);
    }
    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        task,
        blocked_workflow_failure_update(
            "task_delivery_pipeline",
            "jrun-fixture-1",
            Some("delivery_base_checks_pending"),
            Some("required checks also fail on the base"),
        ),
    )
    .unwrap();
    assert_eq!(fx.status(task), "blocked");
}

fn green_fixture(head: &str) -> Fixture {
    let mut fx = fixture();
    fx.runtime = fx.runtime.clone().with_task_pr_forge(Arc::new(GreenForge {
        head: head.to_string(),
    }));
    fx
}

#[test]
fn a_base_red_block_on_the_current_head_requeues_once_the_checks_pass() {
    let fx = green_fixture(HEAD);
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    run_blocks(&fx, &task, Some(HEAD));

    let settled = fx.runtime.recheck_blocked_tasks().unwrap();
    assert_eq!(settled.len(), 1, "{settled:?}");
    assert_eq!(settled[0].kind, RecheckKind::ChecksGreen);
    assert_eq!(fx.status(&task), "backlog");
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry.event == "blocked_task_rechecked"
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.starts_with("kind=checks_green"))),
        "{history:?}"
    );
}

#[test]
fn a_block_that_base_red_history_did_not_cause_stays_blocked_with_green_checks() {
    // Base-red evidence from an earlier run, then a block for another reason.
    let fx = green_fixture(HEAD);
    let earlier = fx.task(&[]);
    fx.record_pull_request(&earlier, 17);
    run_blocks(&fx, &earlier, Some(HEAD));
    fx.update(json!({"id": earlier, "status": "backlog"}));
    run_blocks(&fx, &earlier, None);

    // Base-red evidence for a head the pull request has since left.
    let moved = fx.task(&[]);
    fx.record_pull_request(&moved, 17);
    run_blocks(
        &fx,
        &moved,
        Some("fedcba9876543210fedcba9876543210fedcba98"),
    );

    // Base-red evidence from an earlier run, then a final-recovery escalation.
    let escalated = fx.task(&[]);
    fx.record_pull_request(&escalated, 17);
    run_blocks(&fx, &escalated, Some(HEAD));
    fx.update(json!({"id": escalated, "status": "backlog"}));
    fx.escalate(&escalated);

    assert!(fx.runtime.recheck_blocked_tasks().unwrap().is_empty());
    for task in [&earlier, &moved, &escalated] {
        assert_eq!(fx.status(task), "blocked", "{task}");
    }
}
