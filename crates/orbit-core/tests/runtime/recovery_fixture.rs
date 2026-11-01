//! Shared fixture for the final-recovery integration tests: a workspace with
//! tasks, a way to apply a final-recovery decision to a run of an in-progress
//! task, and a human-acting runtime.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_core::OrbitError;
use orbit_core::application::task::{FinalRecoveryCompletion, FinalRecoveryRequest};
use orbit_core::runtime::task_pr_forge::{
    CheckRerunOutcome, ForgePullRequest, PullRequestBlockers, TaskPrForge,
};
use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::ExternalRef;
use serde_json::{Value, json};
use tempfile::TempDir;

pub(super) struct Fixture {
    _root: TempDir,
    _git_ceiling: orbit_common::test_env::ScopedEnv,
    pub(super) global: PathBuf,
    pub(super) repo: PathBuf,
    pub(super) runtime: OrbitRuntime,
    runs: std::cell::Cell<u32>,
}

pub(super) fn fixture() -> Fixture {
    fixture_with_config("")
}

pub(super) fn fixture_with_config(config: &str) -> Fixture {
    fixture_in(&orbit_common::test_env::bulk_write_temp_dir(), config)
}

/// A fixture rooted under `parent`, so a test can nest the non-Git workspace
/// inside a checkout it builds.
fn fixture_in(parent: &Path, config: &str) -> Fixture {
    let root = TempDir::new_in(parent).unwrap();
    // Git walks past a plain directory when looking for a repository, so a
    // fixture rooted under a checkout (a managed run's nested TMPDIR) would
    // discover the enclosing checkout's `.git` and lock/fetch it. Stop lookup
    // at the fixture root. The canonical spelling matches what `getcwd`
    // reports, including the macOS `/var` to `/private/var` link.
    let ceiling = std::fs::canonicalize(root.path())
        .unwrap_or_else(|_| root.path().to_path_buf())
        .to_string_lossy()
        .into_owned();
    let git_ceiling =
        orbit_common::test_env::scoped([("GIT_CEILING_DIRECTORIES", Some(ceiling.as_str()))]);
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit/auto_tasks")).unwrap();
    if !config.is_empty() {
        std::fs::write(repo.join(".orbit/config.toml"), config).unwrap();
    }
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    Fixture {
        _root: root,
        _git_ceiling: git_ceiling,
        global,
        repo,
        runtime,
        runs: std::cell::Cell::new(0),
    }
}

impl Fixture {
    /// A runtime acting for a named human, over the same roots.
    pub(super) fn human(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global, &self.repo.join(".orbit"))
            .unwrap()
            .with_actor(ActorIdentity::human("human:operator"))
    }

    pub(super) fn task(&self, tags: &[&str]) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Recover an escalated block",
                    "description": "Blocked-task triage fixture task.",
                    "acceptance_criteria": ["Recovered."],
                    "complexity": "low",
                    "type": "chore",
                    "tags": tags,
                    "workspace": self.repo.to_string_lossy(),
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        self.update(json!({"id": id, "plan": "1. Do it."}));
        self.update(json!({"id": id, "status": "backlog"}));
        id
    }

    pub(super) fn update(&self, mut input: Value) {
        input["model"] = json!("codex");
        self.runtime.run_tool("orbit.task.update", input).unwrap();
    }

    pub(super) fn next_run(&self) -> String {
        self.runs.set(self.runs.get() + 1);
        format!("jrun-triage-{}", self.runs.get())
    }

    /// Final recovery's `decision` on a fresh run of the in-progress `task`.
    pub(super) fn final_recovery(&self, task: &str, decision: Value) {
        self.final_recovery_of(task, &self.next_run(), decision);
    }

    /// Final recovery's `decision` on run `run_id` of the in-progress `task`.
    pub(super) fn final_recovery_of(&self, task: &str, run_id: &str, decision: Value) {
        let observed = self
            .runtime
            .final_recovery_revision(&self.runtime.get_task(task).unwrap())
            .unwrap();
        self.runtime
            .apply_final_recovery(
                &FinalRecoveryRequest {
                    task_id: task.to_string(),
                    run_id: run_id.to_string(),
                    observed,
                    repo_root: self.repo.clone(),
                    base_ref: "main".to_string(),
                    completion: FinalRecoveryCompletion::Review,
                    requeue_bound: Default::default(),
                },
                Some(&decision),
            )
            .unwrap();
    }

    /// Start `task` and let final recovery escalate its failed run: one new
    /// escalated block episode.
    pub(super) fn escalate(&self, task: &str) {
        self.update(json!({"id": task, "status": "in_progress"}));
        self.final_recovery(
            task,
            json!({
                "decision": "escalate",
                "diagnosis": "the worker crashed while the host was rebooting",
                "human_action": "requeue once the host is back",
            }),
        );
        assert_eq!(self.status(task), "blocked");
    }

    pub(super) fn status(&self, task: &str) -> String {
        self.show(task)["status"].as_str().unwrap().to_string()
    }

    pub(super) fn show(&self, task: &str) -> Value {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()
    }
}

impl Fixture {
    /// Record pull request `number` on `task`, as delivery records it.
    pub(super) fn record_pull_request(&self, task: &str, number: u64) {
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
}

/// A forge reporting one open pull request, #17, and recording every check
/// rerun it is asked for.
#[derive(Clone)]
pub(super) struct RerunForge {
    blockers: Arc<Mutex<PullRequestBlockers>>,
    reruns: Arc<Mutex<Vec<(u64, String)>>>,
}

impl RerunForge {
    pub(super) fn new(head: &str, failed: &[&str]) -> Self {
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

    pub(super) fn reruns(&self) -> Vec<(u64, String)> {
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
    ) -> Result<CheckRerunOutcome, OrbitError> {
        self.reruns
            .lock()
            .unwrap()
            .push((number, head_sha.to_string()));
        Ok(CheckRerunOutcome {
            requested: vec![(101, "CI".into())],
            uncertain: None,
        })
    }
    fn authenticated_login(&self, _: &Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }
    fn close_pull_request(&self, _: &Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }
}

/// The fixture with `forge` installed.
pub(super) fn fixture_with_forge(forge: &RerunForge) -> Fixture {
    let mut fx = fixture();
    fx.runtime = fx
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    fx
}
