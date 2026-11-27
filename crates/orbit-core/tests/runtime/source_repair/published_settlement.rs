//! Final recovery preserves a timed-out owner publication for external settlement.

use super::*;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest,
};
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_engine::RunLifecycle;
use orbit_types::task::{CANDIDATE_DISCARDED_EVENT, TaskHistoryEntry};
use orbit_types::workflow::POST_DEPLOY_OWED_EVENT;
use std::path::Path;
use std::sync::{Arc, Mutex};

struct MergeObserver {
    merge: Option<MergedPullRequest>,
    before_read: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl TaskPrForge for MergeObserver {
    fn merged_pull_requests(&self, _: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        let before_read = self.before_read.lock().unwrap().take();
        if let Some(update) = before_read {
            update();
        }
        Ok(self.merge.iter().cloned().collect())
    }

    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        panic!("an authenticated publication does not need the forge login")
    }

    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        panic!("there are no other open pull requests")
    }
}

fn runtime_with_merge(
    f: &Fixture,
    merge: Option<MergedPullRequest>,
    before_read: Option<Box<dyn FnOnce() + Send>>,
) -> OrbitRuntime {
    f.runtime
        .clone()
        .with_task_pr_forge(Arc::new(MergeObserver {
            merge,
            before_read: Mutex::new(before_read),
        }))
}

fn requeued_publisher() -> Fixture {
    let (mut f, checkout) = completion::fixture_with_auto_fix(false);
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    // The absolute deadline is already elapsed. No sleeping or live CI is needed.
    state.completion_deadlines.insert("17".into(), 1);
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    let failure = execute_deterministic_action(
        &f.runtime,
        "pr_complete",
        &json!({}),
        &json!({"job_run_id":f.source.run_id,"completed_task_ids":[f.task],
            "workspace_path":checkout,"pr_number":"17","completion":"done",
            "head":f.branch,"published_head_sha":f.head,"base":"main","max_wait_seconds":60}),
        false,
        &HashMap::new(),
        None,
    )
    .unwrap_err();
    assert!(
        failure.to_string().contains("deadline expired"),
        "{failure}"
    );
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Review
    );
    let task = f.runtime.get_task(&f.task).unwrap();
    let recovery = FinalRecoveryRequest {
        task_id: f.task.clone(),
        run_id: f.source.run_id.clone(),
        observed: f.runtime.final_recovery_revision(&task).unwrap(),
        repo_root: f.repo.clone(),
        base_ref: "origin/main".into(),
        completion: FinalRecoveryCompletion::Done,
        requeue_bound: Default::default(),
    };
    assert_eq!(
        f.runtime
            .apply_final_recovery(
                &recovery,
                Some(&json!({
                    "decision": "requeue", "reason": "Completion wait expired with checks pending"
                }))
            )
            .unwrap(),
        FinalRecoveryOutcome::Requeued
    );
    f.source.state = JobRunState::Failed;
    f.source.finished_at = Some(Utc::now());
    f.source.pid = None;
    published_owner::save_run(&f, &f.source);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().status,
        TaskStatus::Backlog
    );
    assert!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .any(|entry| { entry.event == "published_owner_requeued" })
    );
    f
}

fn external_merge(f: &Fixture) -> MergedPullRequest {
    // Produce the recorded commit on the original landing branch.
    git(&f.repo, &["checkout", "-q", "main"]);
    git(
        &f.repo,
        &["merge", "--no-ff", "-m", "Merge delivery", &f.head],
    );
    let commit = git(&f.repo, &["rev-parse", "HEAD"]);
    git(&f.repo, &["push", "-q", "origin", "main"]);
    MergedPullRequest {
        pull: ForgePullRequest {
            number: 17,
            head_branch: f.branch.clone(),
            body: format!("Delivers {}.", f.task),
            author: "orbit-bot".into(),
        },
        url: "https://github.com/owner/repository/pull/17".into(),
        base_branch: "main".into(),
        merged_at: Utc::now(),
        merge_commit: commit,
        head_sha: f.head.clone(),
    }
}

fn final_recovery_publication_settles_through_both_entry_points() {
    for automatic in [false, true] {
        let f = requeued_publisher();
        let unmerged = runtime_with_merge(&f, None, None);
        assert!(unmerged.settle_externally_merged_task(&f.task).is_err());
        assert!(
            unmerged
                .settle_externally_merged_tasks()
                .unwrap()
                .is_empty()
        );

        let merge = external_merge(&f);
        let runtime = runtime_with_merge(&f, Some(merge.clone()), None);
        let count = || {
            runtime
                .list_job_runs(JobRunListParams::default())
                .unwrap()
                .len()
        };
        let before = count();
        let settled = if automatic {
            let mut settled = runtime.settle_externally_merged_tasks().unwrap();
            assert_eq!(settled.len(), 1);
            settled.pop().unwrap()
        } else {
            runtime.settle_externally_merged_task(&f.task).unwrap()
        };
        assert_eq!(settled.task_id, f.task);
        assert_eq!(settled.previous_status, TaskStatus::Backlog);
        assert_eq!(settled.pr_number, merge.pull.number);
        assert_eq!(settled.base_branch, merge.base_branch);
        assert_eq!(settled.merge_commit, merge.merge_commit);
        assert_eq!(count(), before, "settlement starts no new execution");
        let task = runtime.get_task(&f.task).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(task.github_pr_number(), Some("17"));
        assert_eq!(task.job_run_id.as_deref(), Some(f.source.run_id.as_str()));
        let history = runtime.get_task_history(&f.task).unwrap();
        let entry = history
            .iter()
            .find(|entry| {
                entry.event == orbit_core::application::task::EXTERNAL_MERGE_SETTLED_EVENT
            })
            .unwrap();
        assert!(entry.note.as_ref().unwrap().contains(&merge.merge_commit));
    }
}

fn assert_refused(runtime: &OrbitRuntime, id: &str, case: &str) {
    assert!(runtime.settle_externally_merged_task(id).is_err(), "{case}");
    assert!(
        runtime.settle_externally_merged_tasks().unwrap().is_empty(),
        "{case}"
    );
    assert_eq!(
        runtime.get_task(id).unwrap().status,
        TaskStatus::Backlog,
        "{case}"
    );
}

fn requeued_publication_requires_the_exact_forge_identity() {
    let f = requeued_publisher();
    let merge = external_merge(&f);
    for case in ["number", "url", "head branch", "head", "base"] {
        let mut wrong = merge.clone();
        match case {
            "number" => wrong.pull.number += 1,
            "url" => wrong.url = "https://github.com/another/repository/pull/17".into(),
            "head branch" => wrong.pull.head_branch.push_str("-other"),
            "head" => wrong.head_sha.clone_from(&f.base),
            "base" => wrong.base_branch = "release".into(),
            _ => unreachable!(),
        }
        assert_refused(&runtime_with_merge(&f, Some(wrong), None), &f.task, case);
    }
}

fn requeued_publication_keeps_its_lifecycle_and_completion_guards() {
    for case in [
        "spec",
        "discard",
        "successor",
        "live run",
        "live claim",
        "owed evidence",
    ] {
        let f = requeued_publisher();
        let merge = external_merge(&f);
        match case {
            "spec" => {
                f.runtime.run_tool("orbit.task.update", json!({
                    "id":f.task,"description":"Changed after requeue"
                })).unwrap();
            }
            "discard" => append_history(&f, CANDIDATE_DISCARDED_EVENT, None),
            "successor" | "live run" => {
                let mut successor = f.runtime.insert_job_run(
                    "task_pr_pipeline", 1, Utc::now(), Some(json!({"task_ids":[f.task]})), None,
                ).unwrap();
                if case == "successor" {
                    successor.state = JobRunState::Success;
                    successor.finished_at = Some(Utc::now());
                    published_owner::save_run(&f, &successor);
                }
            }
            "live claim" => crate::external_merge_fixture::Fixture::insert_claim(
                &f.runtime, &f.task, "jrun-late-claim", "running",
            ),
            "owed evidence" => append_history(&f, POST_DEPLOY_OWED_EVENT, Some(json!({
                "run_id": f.source.run_id,
                "criteria": [{"criterion":1,"host":"deployment","command":"measure","target":"pass"}],
            }).to_string())),
            _ => unreachable!(),
        }
        assert_refused(&runtime_with_merge(&f, Some(merge), None), &f.task, case);
    }
}

fn append_history(f: &Fixture, event: &str, note: Option<String>) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![
                    TaskHistoryEntry::new(Utc::now(), "system", event).with_note(note),
                ],
                ..Default::default()
            },
        )
        .unwrap();
}

fn changes_during_forge_lookup_refuse_the_settlement_write() {
    for change_status in [false, true] {
        let f = requeued_publisher();
        let merge = external_merge(&f);
        let task = f.task.clone();
        let writer = f.runtime.clone();
        let (status, refusal) = if change_status {
            (TaskStatus::Someday, "its status changed")
        } else {
            (TaskStatus::Backlog, "live `run auto` leaf")
        };
        let runtime = runtime_with_merge(
            &f,
            Some(merge),
            Some(Box::new(move || {
                if change_status {
                    writer
                        .apply_task_automation_update(
                            &task,
                            TaskAutomationUpdate {
                                status: Some(status),
                                ..Default::default()
                            },
                        )
                        .unwrap();
                } else {
                    writer
                        .insert_job_run(
                            "task_auto_pipeline",
                            1,
                            Utc::now(),
                            Some(json!({"task_ids":[task]})),
                            None,
                        )
                        .unwrap();
                }
            })),
        );
        let failure = runtime.settle_externally_merged_task(&f.task).unwrap_err();
        assert!(failure.to_string().contains(refusal), "{failure}");
        assert_eq!(runtime.get_task(&f.task).unwrap().status, status);
    }
}

crate::scenario_table!(
    published_settlement_scenarios,
    final_recovery_publication_settles_through_both_entry_points,
    requeued_publication_requires_the_exact_forge_identity,
    requeued_publication_keeps_its_lifecycle_and_completion_guards,
    changes_during_forge_lookup_refuse_the_settlement_write
);
