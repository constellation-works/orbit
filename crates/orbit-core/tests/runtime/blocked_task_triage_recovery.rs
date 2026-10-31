//! The blocked-task triage's mechanical recoveries through
//! `orbit.task.block_triage`: `resume_run` resumes the episode's own failed
//! run within the requeue bound final recovery shares, `rerun_ci` re-requests
//! the failed checks on the task's pull request at an unchanged head through
//! the runtime's forge and requeues it, and `friction_reported` links or files
//! the friction record on the episode. Each refusal changes nothing and
//! records nothing, so the episode still awaits triage.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_core::OrbitError;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, PullRequestBlockers, TaskPrForge};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{ExternalRef, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::blocked_task_triage::{Fixture, fixture};

const BLOCKER: &str = "the worker crashed while the host was rebooting";
const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// A forge reporting one open pull request, #17, and recording every check
/// rerun it is asked for.
#[derive(Clone)]
struct RerunForge {
    blockers: Arc<Mutex<PullRequestBlockers>>,
    reruns: Arc<Mutex<Vec<(u64, String)>>>,
}

impl RerunForge {
    fn new(head: &str, failed: &[&str]) -> Self {
        Self {
            blockers: Arc::new(Mutex::new(PullRequestBlockers {
                pull: ForgePullRequest {
                    number: 17,
                    head_branch: "orbit/fixture".into(),
                    body: String::new(),
                    author: "orbit-bot".into(),
                },
                head_sha: head.into(),
                conflicting: false,
                failed_required_checks: failed.iter().map(|name| name.to_string()).collect(),
                pending_required_checks: Vec::new(),
                passed_required_checks: Vec::new(),
            })),
            reruns: Arc::default(),
        }
    }

    fn reruns(&self) -> Vec<(u64, String)> {
        self.reruns.lock().unwrap().clone()
    }
}

impl TaskPrForge for RerunForge {
    fn open_pull_requests(&self, _: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(vec![self.blockers.lock().unwrap().pull.clone()])
    }
    fn pull_request_blockers(
        &self,
        _: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        let blockers = self.blockers.lock().unwrap().clone();
        Ok((blockers.pull.number == number).then_some(blockers))
    }
    fn rerun_failed_checks(
        &self,
        _: &Path,
        number: u64,
        head_sha: &str,
    ) -> Result<Vec<String>, OrbitError> {
        self.reruns
            .lock()
            .unwrap()
            .push((number, head_sha.to_string()));
        Ok(vec!["CI".into()])
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

impl Fixture {
    /// Record pull request `number` on `task`, as delivery records it.
    fn record_pull_request(&self, task: &str, number: u64) {
        RuntimeHost::apply_task_automation_update(
            &self.runtime,
            task,
            TaskAutomationUpdate {
                external_refs: vec![ExternalRef::github_pr(number.to_string()).unwrap()],
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn append_history(&self, task: &str, event: &str, note: Value) {
        RuntimeHost::apply_task_automation_update(
            &self.runtime,
            task,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.into(),
                    note: Some(note.to_string()),
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

    fn markers(&self, task: &str) -> Vec<String> {
        self.runtime
            .get_task_history(task)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "blocked_task_triaged")
            .filter_map(|entry| entry.note)
            .collect()
    }

    /// `decision` was refused and nothing was recorded: the task is still
    /// blocked, carries no triage comment or marker, and still qualifies.
    fn assert_refused_unrecorded(&self, task: &str, refusal: &str, names: &str) {
        assert!(
            refusal.contains("refused") && refusal.contains(names),
            "the refusal names its precondition ({names}): {refusal}"
        );
        assert!(
            refusal.contains("nothing was changed or recorded"),
            "{refusal}"
        );
        assert_eq!(self.status(task), "blocked");
        assert!(self.triage_comments(task).is_empty());
        assert!(self.markers(task).is_empty());
        assert!(self.qualifying().contains(&task.to_string()));
    }

    fn rerun_ci(&self, task: &str, head: &str) -> Result<Value, String> {
        self.triage(json!({
            "action": "rerun_ci",
            "id": task,
            "blocker": BLOCKER,
            "head_sha": head,
            "reason": "the failure is a runner network timeout",
        }))
    }

    fn resume_run(&self, task: &str, run_id: &str) -> Result<Value, String> {
        self.triage(json!({
            "action": "resume_run",
            "id": task,
            "blocker": BLOCKER,
            "run_id": run_id,
            "reason": "the provider outage that failed the step has cleared",
        }))
    }
}

/// The fixture with `forge` installed.
fn fixture_with_forge(forge: &RerunForge) -> Fixture {
    let mut fx = fixture();
    fx.runtime = fx
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    fx
}

#[test]
fn rerun_ci_reruns_the_failed_checks_at_the_inspected_head_and_requeues() {
    let forge = RerunForge::new(HEAD, &["Test (shard 1)"]);
    let fx = fixture_with_forge(&forge);
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    fx.escalate(&task);
    let row = &fx.candidates()["qualifying"][0];
    assert_eq!(row["pr_number"], "17", "the candidate names the PR: {row}");

    let answer = fx.rerun_ci(&task, HEAD).unwrap();
    assert_eq!(answer["disposition"], "rerun_ci");
    assert_eq!(answer["outcome"], "requeued");
    assert_eq!(answer["requeue"]["summary"], "requeue 1 of 2 per 24h");
    assert_eq!(answer["applied"]["pr_number"], 17);
    assert_eq!(forge.reruns(), vec![(17, HEAD.to_string())]);
    assert_eq!(fx.status(&task), "backlog");
    let comments = fx.triage_comments(&task);
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert!(
        comments[0].contains("disposition=rerun_ci outcome=requeued")
            && comments[0].contains("Test (shard 1)")
            && comments[0].contains(HEAD),
        "{}",
        comments[0]
    );
    assert_eq!(fx.markers(&task).len(), 1);
}

#[test]
fn rerun_ci_is_refused_unrecorded_when_its_preconditions_fail() {
    let forge = RerunForge::new(HEAD, &["Test (shard 1)"]);
    let fx = fixture_with_forge(&forge);

    // No pull request recorded.
    let no_pr = fx.task(&[]);
    fx.escalate(&no_pr);
    let refusal = fx.rerun_ci(&no_pr, HEAD).unwrap_err();
    fx.assert_refused_unrecorded(&no_pr, &refusal, "records no pull request");

    // The head moved since the triager inspected it.
    let moved = fx.task(&[]);
    fx.record_pull_request(&moved, 17);
    fx.escalate(&moved);
    let refusal = fx
        .rerun_ci(&moved, "fedcba9876543210fedcba9876543210fedcba98")
        .unwrap_err();
    fx.assert_refused_unrecorded(&moved, &refusal, "not the inspected head");

    // The base-red path already owns this head's failure.
    for (event, field) in [
        ("pr_auto_fix_base_red", "failed_head"),
        ("ci_base_suspect", "head"),
    ] {
        let base_red = fx.task(&[]);
        fx.record_pull_request(&base_red, 17);
        fx.append_history(&base_red, event, json!({ field: HEAD }));
        fx.escalate(&base_red);
        let refusal = fx.rerun_ci(&base_red, HEAD).unwrap_err();
        fx.assert_refused_unrecorded(&base_red, &refusal, event);
    }

    // The bound shared with final recovery is exhausted.
    let bounded = fx.task(&[]);
    fx.record_pull_request(&bounded, 17);
    for _ in 0..2 {
        fx.update(json!({"id": bounded, "status": "in_progress"}));
        fx.final_recovery(
            &bounded,
            json!({"decision": "requeue", "reason": "the tool resolves again"}),
        );
    }
    fx.escalate(&bounded);
    let refusal = fx.rerun_ci(&bounded, HEAD).unwrap_err();
    fx.assert_refused_unrecorded(&bounded, &refusal, "limit 2 per 24 hour(s)");

    // No required check failed on the head.
    *forge.blockers.lock().unwrap() = RerunForge::new(HEAD, &[]).blockers.lock().unwrap().clone();
    let green = fx.task(&[]);
    fx.record_pull_request(&green, 17);
    fx.escalate(&green);
    let refusal = fx.rerun_ci(&green, HEAD).unwrap_err();
    fx.assert_refused_unrecorded(&green, &refusal, "no required check failed");

    assert!(forge.reruns().is_empty(), "no refusal reran a check");
}

#[test]
fn friction_reported_links_or_files_the_friction_and_leaves_the_task_blocked() {
    let fx = fixture();
    let existing = fx
        .runtime
        .run_tool(
            "orbit.friction.add",
            json!({
                "title": "Final recovery cannot resume a lost lease",
                "body": "Recovery escalates every lost lease.",
                "model": "claude",
                "workspace": fx.repo.to_string_lossy(),
            }),
        )
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let linked = fx.task(&[]);
    fx.escalate(&linked);
    let answer = fx
        .triage(json!({
            "action": "friction_reported",
            "id": linked,
            "blocker": BLOCKER,
            "friction_id": existing,
            "reason": "move the task to backlog once the lease defect is fixed",
        }))
        .unwrap();
    assert_eq!(answer["disposition"], "friction_reported");
    assert_eq!(answer["outcome"], "reported");
    assert_eq!(answer["applied"]["friction_id"], existing.as_str());
    assert_eq!(answer["applied"]["filed"], false);
    assert_eq!(fx.status(&linked), "blocked");
    let markers = fx.markers(&linked);
    assert_eq!(markers.len(), 1);
    assert!(
        markers[0].contains(&format!("friction={existing}")),
        "{markers:?}"
    );
    assert!(fx.triage_comments(&linked)[0].contains(&format!("friction: {existing}")));

    let filed = fx.task(&[]);
    fx.escalate(&filed);
    let answer = fx
        .triage(json!({
            "action": "friction_reported",
            "id": filed,
            "blocker": BLOCKER,
            "friction_title": "Escalation loses the worker lease",
            "friction_body": "The worker lease is dropped on reboot and never recovered.",
            "reason": "move the task to backlog once the lease defect is fixed",
        }))
        .unwrap();
    assert_eq!(answer["applied"]["filed"], true);
    let id = answer["applied"]["friction_id"].as_str().unwrap();
    let shown = fx
        .runtime
        .run_tool(
            "orbit.friction.show",
            json!({"id": id, "workspace": fx.repo.to_string_lossy()}),
        )
        .unwrap();
    assert_eq!(shown["during_task"], filed.as_str(), "{shown}");
    assert!(fx.markers(&filed)[0].contains(&format!("friction={id}")));
    assert_eq!(fx.status(&filed), "blocked");

    // An unknown friction id is refused and records nothing.
    let unknown = fx.task(&[]);
    fx.escalate(&unknown);
    let refusal = fx
        .triage(json!({
            "action": "friction_reported",
            "id": unknown,
            "blocker": BLOCKER,
            "friction_id": "F2099-01-001",
            "reason": "fix it",
        }))
        .unwrap_err();
    fx.assert_refused_unrecorded(&unknown, &refusal, "no friction record F2099-01-001");
}

#[test]
fn resume_run_is_refused_unrecorded_when_its_preconditions_fail() {
    let fx = fixture();

    // Another run than the episode's.
    let task = fx.task(&[]);
    fx.escalate(&task);
    let refusal = fx.resume_run(&task, "jrun-someone-else").unwrap_err();
    fx.assert_refused_unrecorded(&task, &refusal, "is not the episode's run");

    // The episode's own run, which the resume path cannot resume (no such
    // run exists in this store).
    let episode_run = fx.candidates()["qualifying"][0]["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusal = fx.resume_run(&task, &episode_run).unwrap_err();
    fx.assert_refused_unrecorded(&task, &refusal, &episode_run);

    // A task only an operator recovers.
    let secured = fx.task(&["security"]);
    fx.escalate(&secured);
    let run = fx.candidates()["qualifying"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["task_id"] == secured.as_str())
        .unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusal = fx.resume_run(&secured, &run).unwrap_err();
    fx.assert_refused_unrecorded(&secured, &refusal, "security");

    // The bound shared with final recovery is exhausted.
    let bounded = fx.task(&[]);
    for _ in 0..2 {
        fx.update(json!({"id": bounded, "status": "in_progress"}));
        fx.final_recovery(
            &bounded,
            json!({"decision": "requeue", "reason": "the tool resolves again"}),
        );
    }
    fx.escalate(&bounded);
    let run = fx.candidates()["qualifying"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["task_id"] == bounded.as_str())
        .unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_string();
    let refusal = fx.resume_run(&bounded, &run).unwrap_err();
    fx.assert_refused_unrecorded(&bounded, &refusal, "limit 2 per 24 hour(s)");
}

/// Run `test_name` alone in a child process with its own home, since the
/// substitute pipeline worker it installs is process-wide.
fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_TRIAGE_RESUME_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

#[test]
fn resume_run_resumes_the_episodes_failed_run_within_the_shared_bound() {
    if run_isolated_test(
        "blocked_task_triage_recovery::resume_run_resumes_the_episodes_failed_run_within_the_shared_bound",
    ) {
        return;
    }
    let fx = fixture();
    // Submitted runs execute in a substitute worker that outlives the test.
    let worker = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    crate::worker_fixture::install(worker.path(), "removed");
    let jobs_dir = fx.global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("test_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: test_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();

    let task = fx.task(&[]);
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "test_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task] })),
            None,
        )
        .unwrap();
    // Its worker has exited, as a failed run's has.
    let mut exited = Command::new("true").spawn().unwrap();
    exited.wait().unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), exited.id())
        .unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1))
        .unwrap();
    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        &task,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    fx.final_recovery_of(
        &task,
        &run.run_id,
        json!({
            "decision": "escalate",
            "diagnosis": BLOCKER,
            "human_action": "resume the run once the host is back",
        }),
    );
    assert_eq!(fx.status(&task), "blocked");
    assert_eq!(
        fx.candidates()["qualifying"][0]["run_id"],
        run.run_id.as_str()
    );

    let answer = fx.resume_run(&task, &run.run_id).unwrap();
    assert_eq!(answer["disposition"], "resume_run", "{answer}");
    assert_eq!(answer["outcome"], "resumed");
    assert_eq!(answer["applied"]["summary"], "requeue 1 of 2 per 24h");
    let resumed = jobs.job_run_retries(&run.run_id, 1).unwrap();
    assert_eq!(resumed.len(), 1, "the episode's run was resumed once");
    assert_eq!(
        answer["applied"]["resumed_run_id"],
        resumed[0].run_id.as_str()
    );
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::InProgress,
        "the resume readmitted the task"
    );
    let comments = fx.triage_comments(&task);
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0].contains("disposition=resume_run outcome=resumed")
            && comments[0].contains(&resumed[0].run_id),
        "{}",
        comments[0]
    );
    assert_eq!(fx.markers(&task).len(), 1);
    // The resume counts within the bound final recovery shares.
    assert_eq!(
        fx.runtime
            .get_task_history(&task)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == "blocked_task_triage_resumed")
            .count(),
        1
    );
}
