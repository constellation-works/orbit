#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Task-worktree lifecycle through the engine's deterministic actions.
//!
//! Each test drives the shipped actions over a real fixture repository:
//! `worktree_setup` creates the run's checkout, `pr_prepare` / `git_rebase`
//! carry its candidate onto an advanced base, the `pr_conflict_recovery`
//! leaf finishes a stopped rebase through a substitute provider CLI, and
//! `worktree_gc` decides which checkouts a finished run may give back.
//!
//! The worktree path and every Git call read process-global state (the
//! environment, `$HOME` Git config), so each test body re-runs in an isolated
//! copy of this binary with its own `$HOME` and `$TMPDIR`, no inherited
//! `ORBIT_*` / `GIT_*` variables, and a bounded wait that kills and reaps the
//! child on timeout or panic.

use std::collections::{BTreeMap, HashMap};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_agent::loop_engine::InMemorySink;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, RuntimeHost, TaskAutomationUpdate, V2AuditWriter,
    V2DispatchInput, dispatch_v2_activity, execute_deterministic_action,
};
use orbit_types::task::{ExternalRef, Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Set in the isolated child that runs a test body.
const CHILD_ENV: &str = "ORBIT_WORKTREE_LIFECYCLE_CHILD";
/// Upper bound on one isolated test body, Git calls included.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);
const BASE: &str = "agent-main";

// ---------------------------------------------------------------------------
// worktree_gc
// ---------------------------------------------------------------------------

/// One finished-or-live run whose checkout `worktree_setup` created.
struct GcRun {
    run_id: &'static str,
    state: JobRunState,
    /// Every task the run serves, with the status it holds at collection.
    tasks: &'static [(&'static str, TaskStatus)],
    expected_action: &'static str,
    /// The task the report names: the blocking member, or every member
    /// when the bundle is eligible.
    expected_task: &'static str,
}

#[test]
fn worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones() {
    isolated(
        "worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones",
        || {
            let fixture = Fixture::new();
            let runs = [
                GcRun {
                    run_id: "jrun-gc-blocked",
                    state: JobRunState::Success,
                    tasks: &[("T-GC-BLOCKED", TaskStatus::Blocked)],
                    expected_action: "skipped:task_status_ineligible",
                    expected_task: "T-GC-BLOCKED",
                },
                GcRun {
                    run_id: "jrun-gc-review",
                    state: JobRunState::Success,
                    tasks: &[("T-GC-REVIEW", TaskStatus::Review)],
                    expected_action: "skipped:task_status_ineligible",
                    expected_task: "T-GC-REVIEW",
                },
                GcRun {
                    run_id: "jrun-gc-active",
                    state: JobRunState::Failed,
                    tasks: &[("T-GC-ACTIVE", TaskStatus::InProgress)],
                    expected_action: "skipped:task_status_ineligible",
                    expected_task: "T-GC-ACTIVE",
                },
                GcRun {
                    run_id: "jrun-gc-running",
                    state: JobRunState::Running,
                    tasks: &[("T-GC-RUNNING", TaskStatus::Done)],
                    expected_action: "skipped:run_not_terminal",
                    expected_task: "T-GC-RUNNING",
                },
                GcRun {
                    run_id: "jrun-gc-bundle",
                    state: JobRunState::Success,
                    tasks: &[
                        ("T-GC-BUNDLE-A", TaskStatus::Done),
                        ("T-GC-BUNDLE-B", TaskStatus::Review),
                    ],
                    expected_action: "skipped:task_status_ineligible",
                    expected_task: "T-GC-BUNDLE-B",
                },
                GcRun {
                    run_id: "jrun-gc-done",
                    state: JobRunState::Success,
                    tasks: &[("T-GC-DONE", TaskStatus::Done)],
                    expected_action: "removed",
                    expected_task: "T-GC-DONE",
                },
                GcRun {
                    run_id: "jrun-gc-settled",
                    state: JobRunState::Success,
                    tasks: &[
                        ("T-GC-SETTLED-A", TaskStatus::Done),
                        ("T-GC-SETTLED-B", TaskStatus::Archived),
                    ],
                    expected_action: "removed",
                    expected_task: "T-GC-SETTLED-A,T-GC-SETTLED-B",
                },
            ];

            let host = LifecycleHost::new(&fixture.repo);
            let mut checkouts = Vec::new();
            for run in &runs {
                let task_ids: Vec<&str> = run.tasks.iter().map(|(id, _)| *id).collect();
                for id in &task_ids {
                    host.add_task(id, TaskStatus::Backlog);
                }
                let input = setup_input(&task_ids, run.run_id);
                let setup = action(&host, "worktree_setup", &input).expect("worktree setup");
                for (id, status) in run.tasks {
                    host.set_status(id, *status);
                }
                host.add_run(job_run(run.run_id, run.state, input));
                checkouts.push(Checkout::from_setup(&setup));
            }
            // A checkout no run created is never Orbit's to reclaim.
            let hand_made = fixture.root.path().join("hand-made");
            git(
                &fixture.repo,
                &["worktree", "add", "-b", "scratch", path_str(&hand_made)],
            );

            let result = action(&host, "worktree_gc", &json!({})).expect("worktree gc");

            let reports = result["reports"].as_array().expect("gc reports");
            for (run, checkout) in runs.iter().zip(&checkouts) {
                let report = reports
                    .iter()
                    .find(|report| report["run_id"] == run.run_id)
                    .unwrap_or_else(|| panic!("{}: no gc report in {result:#}", run.run_id));
                assert_eq!(
                    report["action"], run.expected_action,
                    "{}: {report:#}",
                    run.run_id
                );
                assert_eq!(
                    report["task_id"], run.expected_task,
                    "{}: {report:#}",
                    run.run_id
                );
                assert_eq!(
                    Path::new(report["path"].as_str().expect("report path")),
                    checkout.path,
                    "{}: gc resolves the checkout setup created",
                    run.run_id
                );
                let reaped = run.expected_action == "removed";
                assert_eq!(
                    !checkout.path.exists(),
                    reaped,
                    "{}: checkout presence after gc",
                    run.run_id
                );
                assert_eq!(
                    !registered_worktrees(&fixture.repo).contains(&checkout.path),
                    reaped,
                    "{}: worktree registration after gc",
                    run.run_id
                );
                if !reaped {
                    assert_eq!(
                        git(&checkout.path, &["rev-parse", "--abbrev-ref", "HEAD"]),
                        checkout.branch,
                        "{}: a retained checkout keeps its branch",
                        run.run_id
                    );
                }
            }
            assert!(hand_made.exists(), "a hand-made worktree survives gc");
            assert!(registered_worktrees(&fixture.repo).contains(&canonical(&hand_made)));
        },
    );
}

// ---------------------------------------------------------------------------
// worktree_setup: stale-checkout refusal
// ---------------------------------------------------------------------------

#[test]
fn worktree_setup_refuses_a_stale_checkout_and_leaves_it_untouched() {
    isolated(
        "worktree_setup_refuses_a_stale_checkout_and_leaves_it_untouched",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            host.add_task("T-STALE", TaskStatus::Backlog);
            let input = setup_input(&["T-STALE"], "jrun-stale");

            let first = action(&host, "worktree_setup", &input).expect("first setup");
            let checkout = Checkout::from_setup(&first);
            let first_base = first["base_sha"].as_str().unwrap().to_string();

            // Re-running setup against an unchanged base reattaches the same
            // checkout: the refusal below is about the base, not about reuse.
            let again = action(&host, "worktree_setup", &input).expect("reattach at same base");
            assert_eq!(again["workspace_path"], first["workspace_path"]);
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), first_base);
            assert_eq!(host.admitted(), ["T-STALE", "T-STALE"]);

            let retained = commit_file(&checkout.path, "candidate.txt", "inspect me\n");
            let second_base = commit_file(&fixture.repo, "base.txt", "v2\n");

            let error = action(&host, "worktree_setup", &input)
                .expect_err("a checkout behind the new base is refused");
            assert_stale_refusal(&error, &checkout.branch, &retained, &second_base);
            assert_eq!(host.admitted().len(), 2, "a refused setup admits no task");
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), retained);
            assert_eq!(
                fs::read_to_string(checkout.path.join("candidate.txt")).unwrap(),
                "inspect me\n"
            );
            assert!(git(&checkout.path, &["status", "--porcelain"]).is_empty());

            // With the checkout gone, the orphaned branch still holds the
            // unexplained history and is refused the same way.
            git(
                &fixture.repo,
                &["worktree", "remove", "--force", path_str(&checkout.path)],
            );
            let error = action(&host, "worktree_setup", &input)
                .expect_err("an orphan branch behind the new base is refused");
            assert_stale_refusal(&error, &checkout.branch, &retained, &second_base);
            assert!(
                !checkout.path.exists(),
                "a refused orphan attach creates no checkout"
            );
            assert_eq!(
                git(&fixture.repo, &["rev-parse", &checkout.branch]),
                retained
            );
            assert_eq!(host.admitted().len(), 2);
        },
    );
}

// ---------------------------------------------------------------------------
// pr_prepare + git_rebase
// ---------------------------------------------------------------------------

#[test]
fn git_rebase_decision_follows_the_checkout_state() {
    isolated("git_rebase_decision_follows_the_checkout_state", || {
        // A candidate that does not overlap the advanced base is rebased.
        let clean = PreparedRebase::new("jrun-rebase-clean", "candidate.txt", "base.txt");
        let rebased = clean.rebase().expect("non-overlapping rebase");
        assert_eq!(rebased["decision"], "performed", "{rebased:#}");
        assert_ne!(clean.head(), clean.candidate);
        assert!(is_ancestor(&clean.checkout.path, &clean.target, "HEAD"));
        assert_eq!(
            fs::read_to_string(clean.checkout.path.join("candidate.txt")).unwrap(),
            "candidate\n"
        );
        assert!(!rebase_in_progress(&clean.checkout.path));

        // Overlapping edits stop the rebase and hand it to conflict recovery
        // as a typed conflict, with the rebase left in place to resolve.
        let conflicted = PreparedRebase::new("jrun-rebase-conflict", "README.md", "README.md");
        let error = conflicted.rebase().expect_err("overlapping rebase");
        let OrbitError::RecoverableVcsConflict(conflict) = &error else {
            panic!("expected a recoverable conflict, got {error}");
        };
        assert_eq!(conflict.operation, "git_rebase");
        assert_eq!(conflict.target_base_sha, conflicted.target);
        assert_eq!(conflict.original_base_sha, conflicted.base_sha);
        assert_eq!(conflict.conflicting_paths, ["README.md"]);
        assert!(rebase_in_progress(&conflicted.checkout.path));
        assert!(!git(&conflicted.checkout.path, &["ls-files", "-u"]).is_empty());

        // A rebase Orbit did not start is someone else's work: refused and
        // left exactly as found.
        let foreign = PreparedRebase::new("jrun-rebase-foreign", "candidate.txt", "base.txt");
        let rebase_dir = git_path(&foreign.checkout.path, "rebase-merge");
        fs::create_dir_all(&rebase_dir).unwrap();
        for (file, contents) in [
            (
                "orig-head",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n".to_string(),
            ),
            ("onto", format!("{}\n", foreign.target)),
            ("head-name", "refs/heads/foreign-branch\n".to_string()),
            ("git-rebase-todo", String::new()),
            ("end", "1\n".to_string()),
            ("msgnum", "1\n".to_string()),
        ] {
            fs::write(rebase_dir.join(file), contents).unwrap();
        }
        let error = foreign.rebase().expect_err("foreign rebase is refused");
        assert!(
            !matches!(error, OrbitError::RecoverableVcsConflict(_)),
            "a foreign rebase is not a conflict to recover: {error}"
        );
        assert!(
            error.to_string().contains("pre-existing rebase"),
            "the refusal explains itself: {error}"
        );
        assert!(rebase_dir.join("orig-head").is_file());
        assert_eq!(foreign.head(), foreign.candidate);
    });
}

// ---------------------------------------------------------------------------
// pr_conflict_recovery
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn conflict_recovery_leaf_completes_only_its_checkpointed_rebase() {
    isolated(
        "conflict_recovery_leaf_completes_only_its_checkpointed_rebase",
        || {
            let prepared = PreparedRebase::new("jrun-recovery", "README.md", "README.md");
            let target = prepared.target.clone();
            let OrbitError::RecoverableVcsConflict(conflict) =
                prepared.rebase().expect_err("overlapping rebase")
            else {
                panic!("expected a recoverable conflict");
            };

            let provider = prepared.fixture.root.path().join("codex");
            let launched = prepared.fixture.root.path().join("provider-launched");
            write_executable(
                &provider,
                &format!(
                    "#!/bin/sh\nset -eu\ncat > /dev/null\n: > '{}'\nprintf 'candidate and target\\n' > README.md\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
                    launched.display()
                ),
            );
            let host = prepared.host.with_provider(&provider);
            let recovery_input = json!({
                "prompt": "resolve the stopped rebase",
                "task_id": "T-REBASE",
                "workspace_path": prepared.checkout.path,
                "repo_root": prepared.checkout.path,
                "run_id": prepared.run_id,
                "failed_step_id": "sync_base",
                "activity_name": "git_rebase",
                "recovery_kind": "vcs_conflict",
                "operation": conflict.operation,
                "original_base_sha": conflict.original_base_sha,
                "target_base_sha": conflict.target_base_sha,
                "conflicting_paths": conflict.conflicting_paths,
                "failed_step_input": {
                    "head": prepared.prepared["head"],
                    "head_sha": prepared.candidate,
                    "base_ref": prepared.prepared["base_ref"],
                    "base_sha": conflict.target_base_sha,
                },
            });

            // A checkpoint that does not describe the stopped rebase is
            // refused before the provider runs or Git is touched.
            let mut mismatched = recovery_input.clone();
            mismatched["target_base_sha"] = json!(prepared.candidate);
            let error = recover(&host, &prepared.run_id, mismatched)
                .expect_err("mismatched checkpoint is refused");
            assert!(
                error.to_string().contains("existing rebase matching"),
                "{error}"
            );
            assert!(!launched.exists(), "the provider must not launch");
            assert!(rebase_in_progress(&prepared.checkout.path));
            assert!(host.checkpoints().is_empty());

            let outcome = recover(&host, &prepared.run_id, recovery_input)
                .expect("checkpointed rebase is recovered");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(launched.exists());
            assert!(!rebase_in_progress(&prepared.checkout.path));
            assert_eq!(
                fs::read_to_string(prepared.checkout.path.join("README.md")).unwrap(),
                "candidate and target\n"
            );
            assert!(is_ancestor(&prepared.checkout.path, &target, "HEAD"));
            let head = prepared.head();
            let checkpoints = host.checkpoints();
            let [(run_id, step_id, checkpoint)] = checkpoints.as_slice() else {
                panic!("expected one recovery checkpoint, got {checkpoints:#?}");
            };
            assert_eq!(run_id, &prepared.run_id);
            assert_eq!(step_id, "sync_base");
            assert_eq!(checkpoint["head_sha"], head);
            assert_eq!(checkpoint["head_sha_before"], prepared.candidate);
            assert_eq!(checkpoint["base_sha"], target);
            assert_eq!(checkpoint["rewritten"], true);
        },
    );
}

fn recover(
    host: &LifecycleHost,
    run_id: &str,
    input: Value,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let blobs = TempDir::new().unwrap();
    let audit = Arc::new(V2AuditWriter::new(
        run_id,
        "codex:test-model",
        Arc::new(InMemorySink::new(blobs.path().to_path_buf())),
    ));
    let spec = ActivityV2Spec::AgentLoop(AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "Resolve the stopped rebase.".to_string(),
        tools: Vec::new(),
        on_denial: OnDenial::Terminate,
        model: None,
        reasoning_effort: None,
        max_iterations: 1,
        backend: None,
        provider: Provider::Codex,
        wall_clock_timeout_seconds: 30,
        require_response_envelope: false,
        require_completion_envelope: true,
        proc_allowed_programs: None,
        proc_disallowed_programs: None,
        trusted_host_execution: false,
    });
    dispatch_v2_activity(V2DispatchInput {
        activity_name: "pr_conflict_recovery",
        spec: &spec,
        fs_profile: None,
        input,
        audit,
        run_id,
        host: Some(host),
    })
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A primary checkout on `agent-main` whose `.orbit/` scratch is ignored, as
/// in an initialized workspace.
struct Fixture {
    root: TempDir,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init"]);
        git(&repo, &["checkout", "-b", BASE]);
        git(&repo, &["config", "user.name", "Orbit Test"]);
        git(
            &repo,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        fs::write(repo.join("README.md"), "base\n").unwrap();
        fs::write(repo.join("base.txt"), "v1\n").unwrap();
        git(&repo, &["add", ".gitignore", "README.md", "base.txt"]);
        git(&repo, &["commit", "-m", "base"]);
        let origin = root.path().join("origin.git");
        git(root.path(), &["init", "--bare", path_str(&origin)]);
        git(&repo, &["remote", "add", "origin", path_str(&origin)]);
        git(&repo, &["push", "-u", "origin", BASE]);
        let repo = canonical(&repo);
        Self { root, repo }
    }
}

/// What `worktree_setup` reported for a run's checkout.
struct Checkout {
    path: PathBuf,
    branch: String,
}

impl Checkout {
    fn from_setup(output: &Value) -> Self {
        Self {
            path: PathBuf::from(output["workspace_path"].as_str().expect("workspace_path")),
            branch: output["head_ref"].as_str().expect("head_ref").to_string(),
        }
    }
}

/// A run's checkout holding one committed candidate edit, prepared for
/// handoff after the base advanced past the commit it was created from.
struct PreparedRebase {
    fixture: Fixture,
    host: LifecycleHost,
    run_id: String,
    checkout: Checkout,
    base_sha: String,
    candidate: String,
    /// The advanced base `pr_prepare` pinned.
    target: String,
    common: Value,
    prepared: Value,
}

impl PreparedRebase {
    fn new(run_id: &str, candidate_file: &str, base_file: &str) -> Self {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        host.add_task("T-REBASE", TaskStatus::Backlog);
        let setup = action(&host, "worktree_setup", &setup_input(&["T-REBASE"], run_id))
            .expect("worktree setup");
        let checkout = Checkout::from_setup(&setup);
        let base_sha = setup["base_sha"].as_str().unwrap().to_string();
        let candidate = commit_file(&checkout.path, candidate_file, "candidate\n");
        let target = commit_file(&fixture.repo, base_file, "target\n");
        let common = json!({
            "workspace_path": checkout.path,
            "job_run_id": run_id,
            "completed_task_ids": ["T-REBASE"],
            "base": BASE,
            "base_sync": "local",
        });
        let prepared = action(&host, "pr_prepare", &common).expect("pr_prepare");
        Self {
            fixture,
            host,
            run_id: run_id.to_string(),
            checkout,
            base_sha,
            candidate,
            target,
            common,
            prepared,
        }
    }

    fn rebase(&self) -> Result<Value, OrbitError> {
        let mut input = self.common.clone();
        for field in [
            "head",
            "head_sha",
            "base",
            "base_ref",
            "base_sha",
            "remote_sha",
            "commits_behind",
            "sync_required",
        ] {
            input[field] = self.prepared[field].clone();
        }
        action(&self.host, "git_rebase", &input)
    }

    fn head(&self) -> String {
        git(&self.checkout.path, &["rev-parse", "HEAD"])
    }
}

fn setup_input(task_ids: &[&str], run_id: &str) -> Value {
    json!({
        "task_ids": task_ids,
        "run_id": run_id,
        "base": BASE,
        "base_sync": "local",
        "dependency_delivery": "ignore",
    })
}

fn action(host: &LifecycleHost, name: &str, input: &Value) -> Result<Value, OrbitError> {
    execute_deterministic_action(host, name, &json!({}), input, false, &HashMap::new(), None)
}

fn job_run(run_id: &str, state: JobRunState, input: Value) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: run_id.to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: Some(input),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

fn task(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: format!("Fixture task {id}"),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: "Outcome: success\nChanges:\n- Fixture candidate.".to_string(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

/// The task store, run store and provider resolution the lifecycle actions
/// read through, kept in memory.
#[derive(Default)]
struct LifecycleHost {
    repo: PathBuf,
    provider: Option<PathBuf>,
    tasks: Mutex<BTreeMap<String, Task>>,
    runs: Mutex<Vec<JobRun>>,
    admitted: Mutex<Vec<String>>,
    checkpoints: Mutex<Vec<(String, String, Value)>>,
}

impl LifecycleHost {
    fn new(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            ..Self::default()
        }
    }

    /// The same stores, resolving the agent provider to `provider`.
    fn with_provider(&self, provider: &Path) -> Self {
        Self {
            repo: self.repo.clone(),
            provider: Some(provider.to_path_buf()),
            tasks: Mutex::new(self.tasks.lock().unwrap().clone()),
            runs: Mutex::new(self.runs.lock().unwrap().clone()),
            ..Self::default()
        }
    }

    fn add_task(&self, id: &str, status: TaskStatus) {
        self.tasks
            .lock()
            .unwrap()
            .insert(id.to_string(), task(id, status));
    }

    fn set_status(&self, id: &str, status: TaskStatus) {
        self.tasks.lock().unwrap().get_mut(id).unwrap().status = status;
    }

    fn add_run(&self, run: JobRun) {
        self.runs.lock().unwrap().push(run);
    }

    fn admitted(&self) -> Vec<String> {
        self.admitted.lock().unwrap().clone()
    }

    fn checkpoints(&self) -> Vec<(String, String, Value)> {
        self.checkpoints.lock().unwrap().clone()
    }
}

impl RuntimeHost for LifecycleHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn list_tasks_filtered(
        &self,
        _status: Option<TaskStatus>,
        _priority: Option<TaskPriority>,
        _parent_id: Option<&str>,
        _job_run_id: Option<&str>,
        _external_ref: Option<&ExternalRef>,
        _has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self.tasks.lock().unwrap().values().cloned().collect())
    }

    fn admit_task_for_workflow(&self, task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        self.admitted.lock().unwrap().push(task_id.to_string());
        self.set_status(task_id, TaskStatus::InProgress);
        self.get_task(task_id)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(job_run_id) = update.job_run_id {
            task.job_run_id = Some(job_run_id);
        }
        if let Some(status) = update.status {
            task.status = status;
        }
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(self.runs.lock().unwrap().clone())
    }

    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.checkpoints.lock().unwrap().push((
            run_id.to_string(),
            step_id.to_string(),
            output.clone(),
        ));
        Ok(())
    }

    fn validate_step_recovery_mutation(
        &self,
        _run_id: &str,
        _step_id: &str,
        _task_ids: &[String],
        _workspace_path: &Path,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        _input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        Err(DispatchError::DeterministicActionNotRegistered(
            action.to_string(),
        ))
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        let command = self
            .provider
            .as_ref()
            .unwrap_or_else(|| panic!("no substitute CLI configured for provider {provider}"));
        Ok(ResolvedCliExecutor {
            command: command.to_string_lossy().into_owned(),
            args: Vec::new(),
        })
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn orbit_tools::FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        orbit_tools::ToolContext {
            workspace_root: Some(self.repo.clone()),
            ..orbit_tools::ToolContext::default()
        }
    }
}

fn assert_stale_refusal(error: &OrbitError, branch: &str, tip: &str, base: &str) {
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::Execution(_)),
        "expected an execution refusal, got {error:?}"
    );
    assert!(
        message.contains("refusing stale branch"),
        "expected a stale-branch refusal, got {message}"
    );
    assert!(
        message.contains(&format!("'{branch}'")),
        "names {branch}: {message}"
    );
    assert!(
        message.contains(tip),
        "names the retained tip {tip}: {message}"
    );
    assert!(
        message.contains(base),
        "names the requested base {base}: {message}"
    );
}

fn commit_file(repo: &Path, file: &str, contents: &str) -> String {
    fs::write(repo.join(file), contents).unwrap();
    git(repo, &["add", file]);
    git(repo, &["commit", "-m", &format!("write {file}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn registered_worktrees(repo: &Path) -> Vec<PathBuf> {
    git(repo, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect()
}

fn rebase_in_progress(checkout: &Path) -> bool {
    ["rebase-merge", "rebase-apply"]
        .iter()
        .any(|backend| git_path(checkout, backend).is_dir())
}

fn git_path(checkout: &Path, name: &str) -> PathBuf {
    PathBuf::from(git(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    ))
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> bool {
    Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\n{}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap()
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf8 fixture path")
}

// ---------------------------------------------------------------------------
// Isolated child process
// ---------------------------------------------------------------------------

/// Run `body` in a copy of this test binary that sees only the environment
/// the fixture sets, and fail if it fails or outlives [`CHILD_DEADLINE`].
fn isolated(test: &str, body: impl FnOnce()) {
    if std::env::var_os(CHILD_ENV).is_some() {
        body();
        return;
    }
    let sandbox = TempDir::new().unwrap();
    let home = sandbox.path().join("home");
    let tmp = sandbox.path().join("tmp");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    let log_path = sandbox.path().join("child.log");
    let log = fs::File::create(&log_path).unwrap();

    // libtest names a test by its module path below the crate root.
    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([&qualified, "--exact", "--nocapture", "--test-threads=1"])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") {
            command.env_remove(name.as_ref());
        }
    }
    command
        .env(CHILD_ENV, "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("TMPDIR", &tmp)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0");

    let mut child = ReapOnDrop(command.spawn().unwrap());
    let deadline = Instant::now() + CHILD_DEADLINE;
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            drop(child);
            panic!(
                "{test} exceeded {CHILD_DEADLINE:?} in its isolated child:\n{}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "{test} failed in its isolated child ({status}):\n{}",
        fs::read_to_string(&log_path).unwrap_or_default()
    );
}

/// Kills and reaps the child on every exit path, panics included.
struct ReapOnDrop(Child);

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
