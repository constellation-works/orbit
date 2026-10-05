//! A task's terminal decision closes its Orbit-authored PRs through the
//! runtime's forge, and nothing else does.
//!
//! Each test injects a fake forge holding one repository's open PRs: the
//! task's stale delivery PR, its `[BLOCKED]` preservation PR, and PRs that
//! must never be touched (a human's PR on a lookalike branch, another task's
//! PR, a PR whose body does not name the task). The task then moves through
//! the public runtime surface, and the forge's recorded calls are the
//! evidence. The forge contract offers no branch deletion, so a closed PR's
//! branch always remains.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::task_pr_forge::{ForgePullRequest, TaskPrForge};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{ExternalRef, TaskStatus};
use serde_json::{Value, json};
use tempfile::TempDir;

const BOT: &str = "orbit-bot";

#[derive(Default)]
struct ForgeState {
    open: BTreeMap<u64, ForgePullRequest>,
    /// `(number, comment)` for every successful close, in order.
    closed: Vec<(u64, String)>,
    lookups: usize,
    login_lookups: usize,
    fail_list: bool,
    fail_login: bool,
    fail_close: Vec<u64>,
}

#[derive(Default, Clone)]
struct FakeForge(Arc<Mutex<ForgeState>>);

impl FakeForge {
    fn state(&self) -> std::sync::MutexGuard<'_, ForgeState> {
        self.0.lock().unwrap()
    }

    fn open_pr(&self, number: u64, head_branch: &str, body: &str, author: &str) {
        self.state().open.insert(
            number,
            ForgePullRequest {
                number,
                head_branch: head_branch.to_string(),
                body: body.to_string(),
                author: author.to_string(),
            },
        );
    }

    fn closed_numbers(&self) -> Vec<u64> {
        self.state()
            .closed
            .iter()
            .map(|(number, _)| *number)
            .collect()
    }

    fn comment_for(&self, number: u64) -> String {
        self.state()
            .closed
            .iter()
            .find(|(closed, _)| *closed == number)
            .map(|(_, comment)| comment.clone())
            .unwrap_or_else(|| panic!("pull request #{number} was not closed"))
    }

    fn still_open(&self) -> Vec<u64> {
        self.state().open.keys().copied().collect()
    }
}

impl TaskPrForge for FakeForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let mut state = self.state();
        state.lookups += 1;
        if state.fail_list {
            return Err(OrbitError::Execution("gh pr list failed: HTTP 502".into()));
        }
        Ok(state.open.values().cloned().collect())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        let mut state = self.state();
        state.login_lookups += 1;
        if state.fail_login {
            return Err(OrbitError::Execution("gh api user failed: HTTP 401".into()));
        }
        Ok(BOT.to_string())
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError> {
        let mut state = self.state();
        if state.fail_close.contains(&number) {
            return Err(OrbitError::Execution(format!(
                "gh pr close failed for #{number}: HTTP 503"
            )));
        }
        state.open.remove(&number);
        state.closed.push((number, comment.to_string()));
        Ok(())
    }
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: std::path::PathBuf,
    forge: FakeForge,
}

impl Fixture {
    fn new() -> Self {
        Self::with_config(None)
    }

    fn with_config(workspace_config: Option<&str>) -> Self {
        let root = TempDir::new().expect("create tempdir");
        let global_root = root.path().join("global");
        let repo_root = root.path().join("repo");
        let workspace_root = repo_root.join(".orbit");
        std::fs::create_dir_all(&global_root).expect("create global root");
        std::fs::create_dir_all(&workspace_root).expect("create workspace root");
        if let Some(config) = workspace_config {
            std::fs::write(workspace_root.join("config.toml"), config).expect("write config");
        }
        let forge = FakeForge::default();
        let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root)
            .expect("build test runtime")
            .with_task_pr_forge(Arc::new(forge.clone()));
        Self {
            _root: root,
            runtime,
            repo_root,
            forge,
        }
    }

    /// A backlog task with a plan, so it can enter every status.
    fn add_task(&self, title: &str) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": title,
                    "description": "Fixture task whose PRs the forge holds.",
                    "acceptance_criteria": ["Its PRs are handled on terminal decisions."],
                    "complexity": "low",
                    "workspace": self.repo_root.to_string_lossy(),
                    "type": "feature",
                    "model": "codex"
                }),
            )
            .expect("add task");
        let id = task["id"].as_str().expect("task id").to_string();
        self.update(json!({ "id": id, "plan": "1. Deliver it.", "model": "codex" }));
        self.update(json!({ "id": id, "status": "backlog", "model": "codex" }));
        id
    }

    fn update(&self, input: Value) -> Value {
        self.runtime
            .run_tool("orbit.task.update", input)
            .expect("update task")
    }

    /// Record PRs on the task the way delivery automation does.
    fn record_prs(&self, task_id: &str, numbers: &[u64]) {
        self.runtime
            .apply_task_automation_update(
                task_id,
                TaskAutomationUpdate {
                    external_refs: numbers
                        .iter()
                        .map(|number| ExternalRef::github_pr(number.to_string()).unwrap())
                        .collect(),
                    ..TaskAutomationUpdate::default()
                },
            )
            .expect("record PR refs");
    }

    fn move_to_review(&self, task_id: &str) {
        self.update(json!({ "id": task_id, "status": "in_progress", "model": "codex" }));
        self.update(json!({
            "id": task_id,
            "status": "review",
            "execution_summary": "Ready for approval.",
            "model": "codex"
        }));
    }

    /// The task's delivery PR and `[BLOCKED]` PR, plus the PRs no decision
    /// about this task may close.
    fn seed_forge(&self, task_id: &str, other_task_id: &str) {
        let body = format!("Delivers {task_id}.\n\nOrbit run jrun-1");
        self.forge
            .open_pr(11, &format!("orbit/{task_id}-aaaa1111"), &body, BOT);
        self.forge.open_pr(
            13,
            &format!("orbit/{task_id}-bbbb2222"),
            &format!("[BLOCKED] preservation for {task_id}"),
            BOT,
        );
        // A human's PR on a lookalike branch, naming the task.
        self.forge
            .open_pr(20, &format!("orbit/{task_id}-manual"), &body, "alice");
        // Another task's Orbit PR, mentioning this task in passing.
        self.forge.open_pr(
            21,
            &format!("orbit/{other_task_id}-cccc3333"),
            &format!("Delivers {other_task_id}; follows {task_id}."),
            BOT,
        );
        // An Orbit-branch PR whose body names only a longer lookalike ID.
        self.forge.open_pr(
            22,
            &format!("orbit/{task_id}-dddd4444"),
            &format!("Delivers {task_id}0."),
            BOT,
        );
    }

    fn session_event_types(&self) -> Vec<(String, Value)> {
        self.runtime
            .list_session_events(200)
            .expect("session events")
            .into_iter()
            .map(|event| {
                (
                    event.payload["type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    event.payload["data"].clone(),
                )
            })
            .collect()
    }
}

#[test]
fn landing_through_a_pr_closes_the_tasks_other_orbit_prs_naming_the_landing() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Lands through another PR");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[11, 13, 12]);
    fx.move_to_review(&task_id);

    fx.runtime
        .apply_task_automation_update(
            &task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                status_note: Some(
                    "completion authorized by run jrun-9; delivered by pull request #12 merged as \
                     0123456789abcdef0123456789abcdef01234567"
                        .to_string(),
                ),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("complete the task");

    assert_eq!(
        fx.runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Done
    );
    assert_eq!(
        fx.forge.closed_numbers(),
        vec![11, 13],
        "only this task's Orbit-authored PRs close"
    );
    for number in [11, 13] {
        let comment = fx.forge.comment_for(number);
        assert!(
            comment.contains("#12") && comment.contains("0123456789abcdef0123456789abcdef01234567"),
            "the comment names the landing PR and commit: {comment}"
        );
    }
    assert_eq!(
        fx.forge.still_open(),
        vec![20, 21, 22],
        "human, other-task and non-naming PRs stay open"
    );
}

#[test]
fn landing_through_a_commit_names_the_commit() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Lands through a commit");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[13]);
    fx.move_to_review(&task_id);

    fx.update(json!({
        "id": task_id,
        "status": "done",
        "note": "already landed as fedcba9876543210fedcba9876543210fedcba98",
        "model": "codex"
    }));

    assert_eq!(fx.forge.closed_numbers(), vec![11, 13]);
    assert!(
        fx.forge
            .comment_for(13)
            .contains("fedcba9876543210fedcba9876543210fedcba98"),
        "the comment names the landing commit"
    );
}

#[test]
fn approving_a_review_keeps_the_pr_it_was_reviewed_through() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Approved before its PR merged");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    // #11 is the PR the task was promoted to review with: an operator may
    // approve before merging it, so it may still be the landing.
    fx.record_prs(&task_id, &[13, 11]);
    fx.move_to_review(&task_id);

    fx.runtime
        .approve_task(&task_id, None, None)
        .expect("approve the review");

    assert_eq!(fx.forge.closed_numbers(), vec![13]);
    assert!(fx.forge.still_open().contains(&11));
}

#[test]
fn rejecting_closes_the_tasks_orbit_prs_with_the_state_and_reason() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Rejected task");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[11, 13]);
    fx.move_to_review(&task_id);

    fx.runtime
        .reject_task(&task_id, "superseded by a redesign".to_string(), None)
        .expect("reject the task");

    assert_eq!(fx.forge.closed_numbers(), vec![11, 13]);
    let comment = fx.forge.comment_for(13);
    assert!(
        comment.contains("rejected") && comment.contains("superseded by a redesign"),
        "the comment names the state and the reason: {comment}"
    );
    assert_eq!(fx.forge.still_open(), vec![20, 21, 22]);
}

#[test]
fn archiving_closes_the_tasks_orbit_prs_naming_the_state() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Archived task");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[13]);

    fx.runtime.archive_task(&task_id).expect("archive the task");

    assert_eq!(fx.forge.closed_numbers(), vec![11, 13]);
    assert!(fx.forge.comment_for(11).contains("archived"));
}

#[test]
fn a_new_run_leaves_blocked_prs_open() {
    let fx = Fixture::new();
    let task_id = fx.add_task("Blocked then re-run");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[13]);
    fx.update(json!({ "id": task_id, "status": "in_progress", "model": "codex" }));
    fx.runtime
        .apply_task_automation_update(
            &task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                status_note: Some("failure handoff published PR #13".to_string()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("block the task");
    fx.update(json!({ "id": task_id, "status": "backlog", "model": "codex" }));

    fx.runtime
        .start_task(&task_id, None, None)
        .expect("start a new run");

    assert_eq!(
        fx.runtime.get_task(&task_id).unwrap().status,
        TaskStatus::InProgress
    );
    assert!(fx.forge.closed_numbers().is_empty());
    assert_eq!(
        fx.forge.state().lookups,
        0,
        "no forge call before a terminal decision"
    );
    assert!(fx.forge.still_open().contains(&13));
}

#[test]
fn forge_errors_are_warnings_and_never_fail_the_transition() {
    let fx = Fixture::new();
    let listed = fx.add_task("Lookup fails");
    let closing = fx.add_task("One close fails");
    fx.seed_forge(&closing, &listed);
    fx.record_prs(&listed, &[30]);
    fx.record_prs(&closing, &[11, 13]);

    fx.forge.state().fail_list = true;
    fx.runtime
        .archive_task(&listed)
        .expect("a failed PR lookup does not fail the archive");
    assert_eq!(
        fx.runtime.get_task(&listed).unwrap().status,
        TaskStatus::Archived
    );

    fx.forge.state().fail_list = false;
    fx.forge.state().fail_close = vec![11];
    fx.runtime
        .reject_task(&closing, "no longer needed".to_string(), None)
        .expect("a failed close does not fail the rejection");
    assert_eq!(
        fx.runtime.get_task(&closing).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        fx.forge.closed_numbers(),
        vec![13],
        "the other PR still closes"
    );

    let failures = fx
        .session_event_types()
        .into_iter()
        .filter(|(kind, _)| kind == "TaskPullRequestCloseFailed")
        .map(|(_, data)| (data["task_id"].clone(), data["pr_number"].clone()))
        .collect::<Vec<_>>();
    assert!(failures.contains(&(json!(listed), Value::Null)));
    assert!(failures.contains(&(json!(closing), json!(11))));
}

#[test]
fn configured_delivery_authors_replace_the_forge_login() {
    let fx = Fixture::with_config(Some("[pr]\ndelivery_authors = [\"Orbit-Bot\"]\n"));
    let task_id = fx.add_task("Configured identity");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[13]);
    fx.forge.state().fail_login = true;

    fx.runtime.archive_task(&task_id).expect("archive the task");

    assert_eq!(fx.forge.closed_numbers(), vec![11, 13]);
    assert_eq!(fx.forge.state().login_lookups, 0);
}

#[test]
fn the_config_switch_disables_closing() {
    let fx = Fixture::with_config(Some("[pr]\nclose_on_terminal = false\n"));
    let task_id = fx.add_task("Switch off");
    let other = fx.add_task("Unrelated task");
    fx.seed_forge(&task_id, &other);
    fx.record_prs(&task_id, &[11, 13]);

    fx.runtime
        .reject_task(&task_id, "not wanted".to_string(), None)
        .expect("reject the task");

    assert!(fx.forge.closed_numbers().is_empty());
    assert_eq!(fx.forge.state().lookups, 0);
}
