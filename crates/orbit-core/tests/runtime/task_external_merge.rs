//! A task whose delivery PR an operator merged outside Orbit settles to done
//! from that merge, through the owner pass and the operator entry point.
//!
//! Each test injects a fake forge holding the repository's merged PRs: the
//! most recent ones its listing returns, and older ones only a lookup by
//! number reaches. Execution claims are coordination rows written the way
//! the claim allocator writes them, so the store's own guards decide.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::application::task::{EXTERNAL_MERGE_SETTLED_EVENT, TaskAddParams};
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, MergedPullRequest, TaskPrForge};
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::ExternalRef;
use serde_json::json;
use tempfile::TempDir;

const BOT: &str = "orbit-bot";
const MERGE_COMMIT: &str = "03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9";

#[derive(Default)]
struct ForgeState {
    /// What the most-recently-merged listing returns.
    recent: Vec<MergedPullRequest>,
    /// Merged PRs only a lookup by number reaches.
    older: BTreeMap<u64, MergedPullRequest>,
    closed: Vec<u64>,
}

#[derive(Default, Clone)]
struct FakeForge(Arc<Mutex<ForgeState>>);

impl FakeForge {
    fn merged(
        number: u64,
        head: String,
        body: String,
        author: &str,
        base: &str,
    ) -> MergedPullRequest {
        MergedPullRequest {
            pull: ForgePullRequest {
                number,
                head_branch: head,
                body,
                author: author.to_string(),
            },
            base_branch: base.to_string(),
            merge_commit: MERGE_COMMIT.to_string(),
        }
    }

    /// A delivery PR of `task_id`, merged into `base` by `author`.
    fn merge_recent(&self, number: u64, task_id: &str, author: &str, base: &str) {
        self.0.lock().unwrap().recent.push(Self::merged(
            number,
            format!("orbit/{task_id}-47642870"),
            format!("Delivers {task_id}.\n\nOrbit run jrun-1"),
            author,
            base,
        ));
    }
}

impl TaskPrForge for FakeForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn merged_pull_requests(
        &self,
        _repo_root: &Path,
    ) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(self.0.lock().unwrap().recent.clone())
    }

    fn merged_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        let state = self.0.lock().unwrap();
        Ok(state
            .recent
            .iter()
            .chain(state.older.values())
            .find(|merged| merged.pull.number == number)
            .cloned())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Ok(BOT.to_string())
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        _comment: &str,
    ) -> Result<(), OrbitError> {
        self.0.lock().unwrap().closed.push(number);
        Ok(())
    }
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    forge: FakeForge,
    base: String,
}

impl Fixture {
    fn new() -> Self {
        let root =
            TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).expect("create tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let workspace_root = repo_root.join(".orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");
        std::fs::write(repo_root.join("shared.txt"), "fixture\n").expect("write shared file");
        let forge = FakeForge::default();
        let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
            .expect("build test runtime")
            .with_task_pr_forge(Arc::new(forge.clone()));
        let base = runtime.workspace_base_branch().to_string();
        Self {
            _root: root,
            runtime,
            forge,
            base,
        }
    }

    /// A task on `shared.txt` in `status`, with `prs` recorded the way
    /// delivery records them.
    fn add_task(&self, title: &str, status: TaskStatus, prs: &[u64]) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Fixture task: {title}"),
                acceptance_criteria: vec!["Fixture task is observable.".to_string()],
                plan: "Fixture plan.".to_string(),
                context_files: vec!["file:shared.txt".to_string()],
                complexity: TaskComplexity::Low,
                status: Some(status),
                external_refs: prs
                    .iter()
                    .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                    .collect(),
                ..Default::default()
            })
            .expect("add task")
            .id
    }

    /// A running execution claim of `task_id` on `shared.txt`.
    fn claim(&self, task_id: &str, run_id: &str) {
        let claim_id = format!("claim-{run_id}");
        let claim = json!({
            "claim_id": claim_id,
            "task_id": task_id,
            "request_id": format!("req-{run_id}"),
            "executed_on": {"machine_id": "machine-1", "machine_name": null},
            "run_context": {"run_id": run_id, "job_name": "test-job", "machine_name": null},
            "footprint": ["file:shared.txt"],
            "reservation_id": format!("res-{run_id}"),
            "reservation_expires_at": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "phase": "running",
        });
        rusqlite::Connection::open(self.runtime.global_root().join("orbit.db"))
            .unwrap()
            .execute(
                "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
                 VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
                rusqlite::params![
                    self.runtime.workspace_id().unwrap(),
                    claim_id,
                    claim.to_string(),
                    Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }

    /// Bind `task_id` to a running run of this process.
    fn bind_live_run(&self, task_id: &str) {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.runtime.workspace_id().unwrap(),
        );
        let run = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                Utc::now(),
                Some(json!({ "task_ids": [task_id] })),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        self.runtime
            .apply_task_automation_update(
                task_id,
                TaskAutomationUpdate {
                    job_run_id: Some(run.run_id),
                    ..TaskAutomationUpdate::default()
                },
            )
            .expect("bind the run");
    }

    fn status(&self, task_id: &str) -> TaskStatus {
        self.runtime.get_task(task_id).unwrap().status
    }
}

#[test]
fn a_blocked_task_settles_from_its_merged_delivery_pr_while_another_task_claims_its_files() {
    let fx = Fixture::new();
    // ORB-16371: the operator merged the blocked task's delivery PR while
    // another task's run held a claim on the same files.
    let merged = fx.add_task("Merged by hand", TaskStatus::Blocked, &[4937]);
    let own_claim = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[50]);
    let other_branch = fx.add_task("Merged elsewhere", TaskStatus::Blocked, &[51]);
    let not_delivery = fx.add_task("Merged by a human", TaskStatus::Review, &[52, 53]);
    let live_run = fx.add_task("Still running", TaskStatus::InProgress, &[54]);
    let holder = fx.add_task("Claim holder", TaskStatus::InProgress, &[]);
    fx.bind_live_run(&live_run);
    fx.claim(&holder, "jrun-20261017-0625-t1");
    fx.claim(&own_claim, "jrun-own-claim");

    fx.forge.merge_recent(4937, &merged, BOT, &fx.base);
    fx.forge.merge_recent(50, &own_claim, BOT, &fx.base);
    fx.forge.merge_recent(51, &other_branch, BOT, "release");
    fx.forge.merge_recent(52, &not_delivery, "alice", &fx.base);
    fx.forge.0.lock().unwrap().recent.push(FakeForge::merged(
        53,
        format!("orbit/{not_delivery}-dddd4444"),
        format!("Delivers {not_delivery}0."),
        BOT,
        &fx.base,
    ));
    fx.forge.merge_recent(54, &live_run, BOT, &fx.base);

    // The agent-shaped manual close is still a start, and the overlapping
    // claim refuses it: the settlement must not depend on that edge.
    let refused = fx
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({"id": merged, "status": "in-progress", "model": "codex"}),
        )
        .expect_err("a start beside another task's claim is refused");
    assert!(
        refused
            .to_string()
            .contains("task footprint overlaps an execution claim"),
        "{refused}"
    );

    let settled = fx
        .runtime
        .settle_externally_merged_tasks()
        .expect("owner pass");

    assert_eq!(
        settled
            .iter()
            .map(|settlement| (settlement.task_id.as_str(), settlement.pr_number))
            .collect::<Vec<_>>(),
        vec![(merged.as_str(), 4937)],
        "only the delivery PR merged into the target branch settles its task"
    );
    assert_eq!(settled[0].previous_status, TaskStatus::Blocked);
    assert_eq!(settled[0].merge_commit, MERGE_COMMIT);

    let task = fx.runtime.get_task(&merged).unwrap();
    assert_eq!(task.status, TaskStatus::Done);
    assert!(
        task.execution_summary.contains("#4937") && task.execution_summary.contains(MERGE_COMMIT),
        "the summary names the landing: {}",
        task.execution_summary
    );
    let history = fx.runtime.get_task_history(&merged).unwrap();
    let entry = history
        .iter()
        .find(|entry| entry.event == EXTERNAL_MERGE_SETTLED_EVENT)
        .expect("the settlement is recorded in history");
    assert_eq!(entry.from_status, Some(TaskStatus::Blocked));
    assert_eq!(entry.to_status, Some(TaskStatus::Done));
    let note = entry.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("#4937") && note.contains(MERGE_COMMIT) && note.contains(&fx.base),
        "the landing note names the PR, branch and merge commit: {note}"
    );

    for (task_id, status, why) in [
        (&own_claim, TaskStatus::Blocked, "its own claim is live"),
        (
            &other_branch,
            TaskStatus::Blocked,
            "it merged into another branch",
        ),
        (
            &not_delivery,
            TaskStatus::Review,
            "no delivery PR of it merged",
        ),
        (&live_run, TaskStatus::InProgress, "its own run is live"),
        (&holder, TaskStatus::InProgress, "it has no PR"),
    ] {
        assert_eq!(fx.status(task_id), status, "{why}");
    }

    let again = fx
        .runtime
        .settle_externally_merged_tasks()
        .expect("owner pass");
    assert!(again.is_empty(), "a settled task does not settle twice");
}

#[test]
fn the_operator_settles_one_task_from_a_merge_older_than_the_listing() {
    let fx = Fixture::new();
    let reviewed = fx.add_task("Approved on the forge", TaskStatus::Review, &[70, 71]);
    let claimed = fx.add_task("Claimed by its own run", TaskStatus::Blocked, &[80]);
    fx.claim(&claimed, "jrun-own-claim");
    {
        let mut state = fx.forge.0.lock().unwrap();
        state.older.insert(
            71,
            FakeForge::merged(
                71,
                format!("orbit/{reviewed}-aaaa1111"),
                format!("Delivers {reviewed}."),
                BOT,
                &fx.base,
            ),
        );
        state.older.insert(
            80,
            FakeForge::merged(
                80,
                format!("orbit/{claimed}-bbbb2222"),
                format!("Delivers {claimed}."),
                BOT,
                &fx.base,
            ),
        );
    }

    assert!(
        fx.runtime
            .settle_externally_merged_tasks()
            .expect("owner pass")
            .is_empty(),
        "the owner pass reads only the recent listing"
    );
    assert_eq!(fx.status(&reviewed), TaskStatus::Review);

    let settlement = fx
        .runtime
        .settle_externally_merged_task(&reviewed)
        .expect("the operator settles the task");
    assert_eq!(settlement.pr_number, 71);
    assert_eq!(settlement.previous_status, TaskStatus::Review);
    assert_eq!(fx.status(&reviewed), TaskStatus::Done);

    let refused = fx
        .runtime
        .settle_externally_merged_task(&claimed)
        .expect_err("a task with its own live claim is not settled");
    assert!(
        refused.to_string().contains("jrun-own-claim"),
        "the refusal names the task's own claim: {refused}"
    );
    assert_eq!(fx.status(&claimed), TaskStatus::Blocked);

    let refused = fx
        .runtime
        .settle_externally_merged_task(&reviewed)
        .expect_err("a done task does not settle again");
    assert!(refused.to_string().contains("'done'"), "{refused}");
}
