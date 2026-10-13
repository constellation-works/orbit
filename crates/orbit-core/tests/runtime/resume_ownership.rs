//! Resume planning refuses a task bound to another machine's run before any
//! checkpoint, worktree, or validation input is reused. A binding still inside
//! the retry lineage keeps the checkpoint batch id. The attach guard still
//! rejects evidence from a run that does not own the task.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::process::Command;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::application::job::JobRunCancelOutcome;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitError, OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::task::ExecutionLocation;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

const JOB: &str = "resume_binding_fixture";
const LOCAL_MACHINE: &str = "hm_local_resume";
const FOREIGN_MACHINE: &str = "hm_9ca6004473492f06";
const FOREIGN_NAME: &str = "Daniels-Mac-mini.local";

fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_RESUME_OWNERSHIP_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let logs = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let output = orbit_common::test_env::run_child_test(&mut command, test_name, logs.path());
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
    local: ExecutionLocation,
}

impl Fixture {
    fn new() -> Self {
        Self::new_for_job(JOB)
    }

    fn new_for_job(job: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        // The substitute stays alive until this directory is removed, so a
        // resumed run's worker does not exit and interrupt the run mid-assert.
        crate::worker_fixture::install(root.path(), "removed");
        let global = root.path().join("home/.orbit");
        let repo = root.path().join("repo");
        let jobs_dir = global.join("resources/jobs");
        std::fs::create_dir_all(&jobs_dir).unwrap();
        std::fs::create_dir_all(repo.join(".orbit")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        assert_eq!(
            std::path::Path::new(git(&repo, &["rev-parse", "--show-toplevel"]).trim()),
            repo.canonicalize().unwrap(),
        );
        std::fs::write(
            jobs_dir.join(format!("{job}.yaml")),
            format!(
                "schemaVersion: 2\nkind: Job\nmetadata:\n  name: {job}\nspec:\n  state: enabled\n  steps: []\n"
            ),
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let local = ExecutionLocation {
            machine_id: LOCAL_MACHINE.into(),
            machine_name: Some("dk-server-1".into()),
        };
        Self {
            _root: root,
            runtime,
            jobs,
            local,
        }
    }

    fn located(&self, location: ExecutionLocation) -> Arc<dyn JobRunStoreBackend> {
        self.jobs.with_execution_location(Some(location))
    }

    fn task(&self) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: "Resume binding".into(),
                description: "A delivery whose binding resume must honor.".into(),
                acceptance_criteria: vec!["Resume honors the binding.".into()],
                plan: "1. Resume only the owning lineage.".into(),
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Bug),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    fn bind(&self, task: &str, run: &str) {
        self.runtime
            .apply_task_automation_update(
                task,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    job_run_id: Some(run.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// A failed run whose reused worktree checkpoint and stored input name `batch`.
    fn failed_run(
        &self,
        jobs: &dyn JobRunStoreBackend,
        task: &str,
        batch: &str,
        parent: Option<&str>,
    ) -> String {
        let input = json!({
            "task_ids": [task],
            "job_run_id": batch,
            "completion": "done",
        });
        let run = jobs
            .insert_job_run(
                JOB,
                1,
                Utc::now(),
                Some(json!({"task_ids": [task]})),
                parent.map(str::to_string),
            )
            .unwrap();
        self.stamp_input(&run.run_id, &input);
        let mut state = PipelineState::new(run.run_id.clone(), JOB.into(), input);
        let worktree = json!({
            "job_run_id": batch,
            "batch_id": batch,
            "workspace_path": "/fixture/worktree",
            "task_ids": [task],
        });
        state.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
        state.record_pipeline_output("worktree", worktree);
        jobs.write_run_state(&run.run_id, &state).unwrap();
        jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        jobs.finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1u64))
            .unwrap();
        run.run_id
    }

    fn stamp_input(&self, run: &str, input: &Value) {
        let workspace = self.runtime.workspace_id().unwrap();
        self.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET input_json=?3 WHERE workspace_id=?1 AND run_id=?2",
                        rusqlite::params![workspace, run, input.to_string()],
                    )
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                Ok(())
            })
            .unwrap();
    }
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .current_dir(repo)
        .env("GIT_ALLOW_PROTOCOL", "file")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Two deterministic stops with real resume admission in between. The worker
/// substitute holds the resumed owner alive while we checkpoint its reviewer;
/// no provider timing or concurrent editing is needed.
#[cfg(unix)]
#[test]
fn checkpoint_resume_preserves_the_later_interrupted_review_repair() {
    if run_isolated_test(
        "resume_ownership::checkpoint_resume_preserves_the_later_interrupted_review_repair",
    ) {
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    std::fs::write(
        home.join(".gitconfig"),
        "[protocol]\nallow = never\n[protocol \"file\"]\nallow = always\n",
    )
    .unwrap();
    // Older already-resumed runs can still contain a carried record. Check
    // that guard too, including a different step/attempt on the current run.
    for stale in ["none", "ancestor", "attempt", "step", "legacy"] {
        let job = "task_pr_pipeline";
        let fx = Fixture::new_for_job(job);
        let repo = fx._root.path().join("repo");
        git(&repo, &["config", "user.name", "fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.test"]);
        std::fs::write(repo.join("repair.rs"), "// candidate\n").unwrap();
        git(&repo, &["add", "repair.rs"]);
        git(&repo, &["commit", "-q", "-m", "candidate"]);
        let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
        let origin = fx._root.path().join("origin.git");
        git(&repo, &["init", "--bare", "-q", origin.to_str().unwrap()]);
        git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let task = fx.task();
        let source = fx
            .located(fx.local.clone())
            .insert_job_run(job, 1, Utc::now(), Some(json!({"task_ids": [task]})), None)
            .unwrap();
        fx.bind(&task, &source.run_id);
        let mut checkpoint = PipelineState::new(
            source.run_id.clone(),
            job.into(),
            source.input.clone().unwrap(),
        );
        let worktree = json!({
            "workspace_path": repo, "job_run_id": source.run_id, "task_ids": [task],
        });
        checkpoint.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
        checkpoint.record_pipeline_output("worktree", worktree);
        checkpoint.record_pipeline_output(
            "review_gate_admit",
            json!({"applies": true, "attempt_id": "first-attempt"}),
        );
        fx.jobs
            .write_run_state(&source.run_id, &checkpoint)
            .unwrap();
        // A staged edit and a later unstaged edit must both be kept without
        // changing the candidate's real index.
        std::fs::write(repo.join("repair.rs"), "// first repair\n").unwrap();
        git(&repo, &["add", "repair.rs"]);
        let index = git(&repo, &["write-tree"]);
        fx.jobs
            .mark_job_run_running(
                &source.run_id,
                Utc::now() - chrono::Duration::minutes(10),
                999_999,
            )
            .unwrap();
        assert_eq!(
            fx.runtime.show_job_run(&source.run_id).unwrap().state,
            JobRunState::Interrupted,
        );
        let first_state = fx.jobs.read_run_state(&source.run_id).unwrap().unwrap();
        let first = first_state.pipeline["interrupted_review"].clone();
        assert_eq!(first["run_id"], source.run_id, "{stale}: {first}");
        let first_ref = first["repair_ref"].as_str().unwrap();
        let first_commit = first["repair_commit"].as_str().unwrap();
        assert_eq!(
            first_ref,
            format!("refs/orbit/candidates/{task}/{}-review", source.run_id)
        );
        assert_eq!(
            git(&repo, &["show", &format!("{first_ref}:repair.rs")]),
            "// first repair\n"
        );

        let resumed = fx
            .runtime
            .submit_resume_run(&source.run_id, Some("operator"), None)
            .expect("resume the interrupted owner from its checkpoints");
        let mut seeded = fx.jobs.read_run_state(&resumed.run_id).unwrap().unwrap();
        assert_eq!(seeded.step_output(0), first_state.step_output(0));
        assert!(
            seeded.pipeline.get("interrupted_review").is_none(),
            "{stale}"
        );
        assert_eq!(
            fx.jobs
                .get_job_run(&resumed.run_id)
                .unwrap()
                .unwrap()
                .retry_source_run_id,
            Some(source.run_id.clone()),
        );
        if stale != "none" {
            let mut inherited = first.clone();
            match stale {
                "attempt" => inherited["run_id"] = json!(resumed.run_id),
                "step" => {
                    inherited["run_id"] = json!(resumed.run_id);
                    inherited["attempt_id"] = json!("second-attempt");
                    inherited["step_id"] = json!("landing_review");
                }
                "legacy" => {
                    inherited.as_object_mut().unwrap().remove("run_id");
                }
                _ => {}
            }
            seeded.pipeline["interrupted_review"] = inherited;
        }
        fx.jobs.write_run_state(&resumed.run_id, &seeded).unwrap();
        // The live engine checkpoint merges keys into the resumed pipeline;
        // an inherited record survives this write and must not suppress B.
        fx.runtime
            .checkpoint_step(
                &resumed.run_id,
                1,
                "review_gate_admit",
                &json!({"applies": true, "attempt_id": "second-attempt"}),
                &Default::default(),
            )
            .unwrap();
        std::fs::write(repo.join("repair.rs"), "// first repair\n// later repair\n").unwrap();
        std::fs::write(repo.join("later.rs"), "// later untracked repair\n").unwrap();
        let status = git(&repo, &["status", "--porcelain"]);
        let cancel = fx.runtime.cancel_job_run(&resumed.run_id).unwrap();
        assert_eq!(cancel.final_state, "cancelled", "{stale}: {cancel:?}");
        let second_state = fx.jobs.read_run_state(&resumed.run_id).unwrap().unwrap();
        let second = &second_state.pipeline["interrupted_review"];
        assert_eq!(second["run_id"], resumed.run_id, "{stale}: {second}");
        assert_eq!(second["step_id"], "review", "{stale}: {second}");
        assert_eq!(second["attempt_id"], "second-attempt", "{stale}: {second}");
        assert_eq!(second["carried"], true, "{stale}: {second}");
        let second_ref = second["repair_ref"].as_str().unwrap();
        assert_eq!(
            second_ref,
            format!("refs/orbit/candidates/{task}/{}-review", resumed.run_id)
        );
        assert_ne!(second_ref, first_ref, "{stale}");
        assert_ne!(second["repair_commit"], first["repair_commit"], "{stale}");
        assert_eq!(
            git(&origin, &["show", &format!("{second_ref}:repair.rs")]),
            "// first repair\n// later repair\n"
        );
        assert_eq!(
            git(&origin, &["show", &format!("{second_ref}:later.rs")]),
            "// later untracked repair\n"
        );
        assert_eq!(
            git(&repo, &["rev-parse", &format!("{second_ref}^")]).trim(),
            head
        );
        assert_eq!(git(&repo, &["rev-parse", first_ref]).trim(), first_commit);
        assert_eq!(
            fx.jobs.read_run_state(&source.run_id).unwrap().unwrap(),
            first_state
        );
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), head);
        assert_eq!(git(&repo, &["branch", "--show-current"]).trim(), "main");
        assert_eq!(git(&repo, &["write-tree"]), index);
        assert_eq!(git(&repo, &["status", "--porcelain"]), status);
        let comments = fx.runtime.get_task_comments(&task).unwrap();
        for repair_ref in [first_ref, second_ref] {
            assert_eq!(
                comments
                    .iter()
                    .filter(|comment| comment.message.contains(repair_ref))
                    .count(),
                1,
                "{stale}: {comments:?}"
            );
        }
        // Repeated stop requests must not mint another repair or comment.
        assert_eq!(
            fx.runtime.cancel_job_run(&resumed.run_id).unwrap().outcome,
            JobRunCancelOutcome::AlreadyTerminal
        );
        assert_eq!(
            fx.jobs
                .read_run_state(&resumed.run_id)
                .unwrap()
                .unwrap()
                .pipeline["interrupted_review"],
            *second
        );
        assert_eq!(fx.runtime.get_task_comments(&task).unwrap(), comments);
        assert_eq!(
            git(&repo, &["rev-parse", second_ref]).trim(),
            second["repair_commit"].as_str().unwrap()
        );
    }
}

#[test]
fn resume_refuses_a_task_bound_to_another_machines_run_before_any_step() {
    if run_isolated_test(
        "resume_ownership::resume_refuses_a_task_bound_to_another_machines_run_before_any_step",
    ) {
        return;
    }
    let fx = Fixture::new();
    let task = fx.task();
    let local_jobs = fx.located(fx.local.clone());
    let source = fx.failed_run(&*local_jobs, &task, "placeholder", None);
    let source_input = json!({
        "task_ids": [task],
        "job_run_id": source,
        "completion": "done",
    });
    fx.stamp_input(&source, &source_input);
    let mut owned = PipelineState::new(source.clone(), JOB.into(), source_input);
    let worktree = json!({
        "job_run_id": source,
        "batch_id": source,
        "workspace_path": "/fixture/worktree",
        "task_ids": [task],
    });
    owned.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    owned.record_pipeline_output("worktree", worktree);
    fx.jobs.write_run_state(&source, &owned).unwrap();
    let foreign = ExecutionLocation {
        machine_id: FOREIGN_MACHINE.into(),
        machine_name: Some(FOREIGN_NAME.into()),
    };
    let foreign_run = fx.failed_run(&*fx.located(foreign.clone()), &task, "foreign-batch", None);
    fx.bind(&task, &foreign_run);
    let bound = fx.runtime.get_task(&task).unwrap();
    assert_eq!(bound.job_run_id.as_deref(), Some(foreign_run.as_str()));
    assert_eq!(bound.job_run_machine.as_ref(), Some(&foreign));
    let runs_before = fx.jobs.list_job_runs(JOB).unwrap();
    let state_before = fx.jobs.read_run_state(&source).unwrap().unwrap();
    let history_before = fx.runtime.get_task_history(&task).unwrap();
    let artifacts_before = fx.runtime.get_task_artifacts(&task).unwrap();

    let error = fx
        .runtime
        .submit_resume_run(&source, Some("operator"), None)
        .expect_err("a foreign binding must refuse resume");

    let OrbitError::JobValidation(message) = error else {
        panic!("resume must refuse before inserting a run: {error}");
    };
    assert!(message.contains(&task), "{message}");
    assert!(message.contains(&foreign_run), "{message}");
    assert!(message.contains(FOREIGN_MACHINE), "{message}");
    assert!(message.contains(FOREIGN_NAME), "{message}");
    assert!(message.contains(&source), "{message}");
    assert!(
        message.contains("authorized rebind"),
        "the refusal must name the supported next action: {message}"
    );
    assert!(
        message.contains("does not overwrite a foreign claim"),
        "{message}"
    );
    assert_eq!(
        fx.jobs.list_job_runs(JOB).unwrap().len(),
        runs_before.len(),
        "resume must not insert a run"
    );
    assert!(
        fx.jobs
            .list_job_runs(JOB)
            .unwrap()
            .iter()
            .all(|run| run.retry_source_run_id.is_none()),
        "no resumed run is linked"
    );
    assert_eq!(
        fx.jobs.read_run_state(&source).unwrap().unwrap(),
        state_before,
        "source checkpoints stay unread for a new run"
    );
    let task_after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(task_after.job_run_id, bound.job_run_id);
    assert_eq!(task_after.job_run_machine, bound.job_run_machine);
    assert_eq!(task_after.status, bound.status);
    assert_eq!(fx.runtime.get_task_history(&task).unwrap(), history_before);
    assert_eq!(
        fx.runtime.get_task_artifacts(&task).unwrap(),
        artifacts_before,
        "validation evidence is not attached"
    );
}

#[test]
fn resume_of_a_lineage_binding_keeps_the_checkpoint_batch_id() {
    if run_isolated_test(
        "resume_ownership::resume_of_a_lineage_binding_keeps_the_checkpoint_batch_id",
    ) {
        return;
    }
    let fx = Fixture::new();
    let local_jobs = fx.located(fx.local.clone());

    // Still bound to the source run: admit, and do not rewrite ownership ids
    // onto the new run.
    let source_task = fx.task();
    let source = fx.failed_run(&*local_jobs, &source_task, "placeholder", None);
    let source_input = json!({
        "task_ids": [source_task],
        "job_run_id": source,
        "completion": "done",
    });
    fx.stamp_input(&source, &source_input);
    let mut source_state = PipelineState::new(source.clone(), JOB.into(), source_input);
    let worktree = json!({
        "job_run_id": source,
        "batch_id": source,
        "task_ids": [source_task],
    });
    source_state.record_step(0, JobRunState::Success, Some(worktree.clone()), None);
    source_state.record_pipeline_output("worktree", worktree);
    fx.jobs.write_run_state(&source, &source_state).unwrap();
    fx.bind(&source_task, &source);
    let admitted = fx
        .runtime
        .submit_resume_run(&source, Some("operator"), None)
        .expect("a task bound to its source run still resumes");
    let seeded = fx.jobs.read_run_state(&admitted.run_id).unwrap().unwrap();
    assert_eq!(
        seeded.run_id, admitted.run_id,
        "the document id is re-keyed"
    );
    assert_eq!(
        seeded.step_output(0).unwrap()["job_run_id"],
        json!(source),
        "checkpoint ownership stays the batch that created the worktree"
    );
    assert_eq!(seeded.initial_input["job_run_id"], json!(source));
    assert_eq!(
        fx.jobs
            .get_job_run(&admitted.run_id)
            .unwrap()
            .unwrap()
            .input
            .unwrap()["job_run_id"],
        json!(source)
    );
    let rebound = fx.runtime.get_task(&source_task).unwrap();
    assert_eq!(rebound.job_run_id.as_deref(), Some(source.as_str()));
    assert_eq!(rebound.job_run_machine.as_ref(), Some(&fx.local));

    // Re-keyed flow: the task is bound to a descendant, checkpoints name the
    // ancestor batch, and reconcile restamps onto that batch without rewriting
    // the checkpoint ownership id onto the new run.
    let batch_task = fx.task();
    let ancestor = fx.failed_run(&*local_jobs, &batch_task, "placeholder", None);
    let ancestor_input = json!({
        "task_ids": [batch_task],
        "job_run_id": ancestor,
        "completion": "done",
    });
    fx.stamp_input(&ancestor, &ancestor_input);
    let mut ancestor_state = PipelineState::new(ancestor.clone(), JOB.into(), ancestor_input);
    let ancestor_output = json!({
        "job_run_id": ancestor,
        "batch_id": ancestor,
        "task_ids": [batch_task],
    });
    ancestor_state.record_step(0, JobRunState::Success, Some(ancestor_output.clone()), None);
    ancestor_state.record_pipeline_output("worktree", ancestor_output);
    fx.jobs.write_run_state(&ancestor, &ancestor_state).unwrap();
    let descendant = fx.failed_run(
        &*local_jobs,
        &batch_task,
        ancestor.as_str(),
        Some(ancestor.as_str()),
    );
    fx.bind(&batch_task, &descendant);
    let resumed = fx
        .runtime
        .submit_resume_run(&descendant, Some("operator"), None)
        .expect("a lineage restamp still resumes");
    let seeded = fx.jobs.read_run_state(&resumed.run_id).unwrap().unwrap();
    assert_eq!(
        seeded.step_output(0).unwrap()["job_run_id"],
        json!(ancestor)
    );
    assert_eq!(seeded.initial_input["job_run_id"], json!(ancestor));
    assert_ne!(seeded.run_id, ancestor);
    assert_eq!(
        fx.runtime
            .get_task(&batch_task)
            .unwrap()
            .job_run_id
            .as_deref(),
        Some(ancestor.as_str()),
        "lineage reconcile restamps onto the checkpoint batch"
    );
}

#[test]
fn a_run_that_does_not_own_the_task_cannot_attach_validation_evidence() {
    if run_isolated_test(
        "resume_ownership::a_run_that_does_not_own_the_task_cannot_attach_validation_evidence",
    ) {
        return;
    }
    let fx = Fixture::new();
    let task = fx.task();
    let owner = fx.failed_run(&*fx.located(fx.local.clone()), &task, "owner", None);
    fx.bind(&task, &owner);
    let path = "validation-log.json";
    fx.runtime
        .attach_task_validation_log(&task, &owner, path, br#"{"ok":true}"#.to_vec())
        .expect("the owning run still attaches validation evidence");
    assert_eq!(fx.runtime.get_task_artifacts(&task).unwrap().len(), 1);

    let other = "jrun-foreign-owner";
    let error = fx
        .runtime
        .attach_task_validation_log(&task, other, path, br#"{"ok":false}"#.to_vec())
        .expect_err("a non-owning run cannot attach validation evidence");
    let OrbitError::PolicyDenied(message) = error else {
        panic!("the attach guard must stay a policy denial: {error}");
    };
    assert_eq!(
        message,
        format!("run '{other}' does not own task '{task}'; it cannot attach validation evidence")
    );
    let artifacts = fx.runtime.get_task_artifacts(&task).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, path);
    assert_eq!(artifacts[0].content, br#"{"ok":true}"#);
}
