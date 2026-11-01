//! The deterministic blocked-task recheck through its public boundary: a
//! blocked task whose delivery pull request merged outside Orbit settles to
//! `done` without an agent, the recheck records why, and a task whose pull
//! request is still unmerged is left blocked for final recovery.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::recovery_fixture::fixture;
use orbit_core::OrbitError;
use orbit_core::application::task::RecheckKind;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};

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
