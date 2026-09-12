//! Shipped task-pilot job boundary regressions [ORB-11411].

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_engine::{DispatchError, JobOutcome, ResolvedCliExecutor, RuntimeHost};
use orbit_store::V2AuditEventFilter;
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{ExecutorDef, ExecutorType, JobRunState, JobRunTrigger};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::exec::{seed_default_catalogs, try_execute_named_job};
use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

struct TaskPilotJobFixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    repo_root: PathBuf,
    global_root: PathBuf,
    stale_sha: String,
    current_sha: String,
    dirty_status: String,
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn configure_commits(repo: &Path) {
    git(repo, &["config", "user.name", "Orbit Test"]);
    git(repo, &["config", "user.email", "orbit-test@example.com"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

fn commit_file(repo: &Path, relative: &str, contents: &str) -> String {
    let path = repo.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(&path, contents).expect("write fixture file");
    git(repo, &["add", relative]);
    git(repo, &["commit", "-m", &format!("write {relative}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn task_pilot_job_fixture(config_branch: &str, remote_branch: &str) -> TaskPilotJobFixture {
    let config = format!("[workflow]\nbase_branch = {config_branch:?}\n");
    let root = tempfile::tempdir().expect("create fixture root");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    fs::write(workspace_root.join("config.toml"), config).expect("write workspace config");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    seed_default_catalogs(&global_root);

    let remote = root.path().join("remote.git");
    let publisher = root.path().join("publisher");
    let remote_path = remote.to_str().expect("UTF-8 remote path");
    let publisher_path = publisher.to_str().expect("UTF-8 publisher path");

    git(root.path(), &["init", "--bare", remote_path]);
    git(&repo_root, &["init"]);
    git(&repo_root, &["checkout", "-b", remote_branch]);
    configure_commits(&repo_root);
    fs::write(repo_root.join(".gitignore"), ".orbit/\n").expect("ignore workspace state");
    fs::create_dir_all(repo_root.join("src")).expect("create source directory");
    fs::write(repo_root.join("src/base.rs"), "base\n").expect("write base source");
    git(&repo_root, &["add", ".gitignore", "src/base.rs"]);
    git(&repo_root, &["commit", "-m", "seed source"]);
    git(&repo_root, &["remote", "add", "origin", remote_path]);
    git(&repo_root, &["push", "-u", "origin", remote_branch]);
    let stale_sha = git(&repo_root, &["rev-parse", "HEAD"]);

    git(
        root.path(),
        &[
            "clone",
            "--branch",
            remote_branch,
            remote_path,
            publisher_path,
        ],
    );
    configure_commits(&publisher);
    let current_sha = commit_file(&publisher, "src/remote.rs", "remote update\n");
    git(&publisher, &["push", "origin", remote_branch]);

    fs::write(repo_root.join("src/base.rs"), "dirty primary\n").expect("dirty primary source");
    fs::write(repo_root.join("src/untracked.rs"), "untracked\n").expect("write untracked source");
    let dirty_status = git(&repo_root, &["status", "--short"]);

    TaskPilotJobFixture {
        _root: root,
        runtime,
        repo_root,
        global_root,
        stale_sha,
        current_sha,
        dirty_status,
    }
}

fn execute_task_pilot_job(
    fixture: &TaskPilotJobFixture,
    input: Value,
    run_id: &str,
) -> Result<JobOutcome, orbit_engine::DispatchError> {
    try_execute_named_job(
        &fixture.runtime,
        &fixture.repo_root,
        &fixture.runtime,
        "task_pilot_pipeline",
        input,
        run_id,
    )
}

fn assert_job_resolves_branch(config_branch: &str, input: Value, expected_branch: &str) {
    let fixture = task_pilot_job_fixture(config_branch, expected_branch);
    let run_id = fixture
        ._root
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture root has a UTF-8 basename");
    let outcome = execute_task_pilot_job(&fixture, input, run_id)
        .expect("shipped task-pilot job must render and reach preparation");

    assert!(outcome.success, "pipeline outcome: {outcome:?}");
    assert_eq!(
        outcome.pipeline["prepare"]["source"]["base_branch"],
        expected_branch
    );
    assert_eq!(
        outcome.pipeline["prepare"]["source"]["source_revision"],
        fixture.current_sha
    );
    assert_eq!(
        git(&fixture.repo_root, &["rev-parse", "HEAD"]),
        fixture.stale_sha,
        "preparation must not advance the primary checkout"
    );
    assert_eq!(
        git(&fixture.repo_root, &["status", "--short"]),
        fixture.dirty_status,
        "preparation must preserve dirty and untracked primary files"
    );
    assert!(!fixture.repo_root.join("src/remote.rs").exists());
}

#[test]
fn shipped_task_pilot_job_renders_omitted_and_empty_workspace_branch_inputs() {
    for (config_branch, input) in [
        ("main", json!({})),
        ("main", json!({ "base_branch": String::new() })),
        ("agent-main", json!({})),
        ("agent-main", json!({ "base_branch": String::new() })),
    ] {
        assert_job_resolves_branch(config_branch, input, config_branch);
    }
}

#[test]
fn shipped_task_pilot_job_honors_an_explicit_alternate_branch() {
    let alternate_branch = format!("alternate-{}", std::process::id());

    assert_job_resolves_branch(
        "main",
        json!({ "base_branch": alternate_branch.clone() }),
        &alternate_branch,
    );
}

#[test]
fn shipped_task_pilot_job_rejects_an_unavailable_explicit_branch() {
    let fixture = task_pilot_job_fixture("main", "main");
    let run_id = fixture
        ._root
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("fixture root has a UTF-8 basename");
    let error =
        execute_task_pilot_job(&fixture, json!({ "base_branch": "missing-branch" }), run_id)
            .expect_err("an unavailable explicit branch must fail before pilot dispatch");
    let message = error.to_string();

    assert!(message.contains("could not fetch"), "{message}");
    assert!(message.contains("missing-branch"), "{message}");
    assert_eq!(
        git(&fixture.repo_root, &["rev-parse", "HEAD"]),
        fixture.stale_sha
    );
    assert_eq!(
        git(&fixture.repo_root, &["status", "--short"]),
        fixture.dirty_status
    );
}

struct ScriptedPilotHost<'a> {
    runtime: &'a OrbitRuntime,
    calls: Mutex<Vec<bool>>,
}

impl RuntimeHost for ScriptedPilotHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        if action != "scripted_task_pilot" {
            return <OrbitRuntime as RuntimeHost>::run_deterministic(
                self.runtime,
                action,
                config,
                input,
                tool_context,
            );
        }
        let repair = input["repair_attempt"].as_bool().unwrap_or(false);
        self.calls.lock().expect("calls").push(repair);
        let task_ids = input["task_ids"].as_array().expect("task ids");
        let tasks = task_ids
            .iter()
            .enumerate()
            .map(|(index, task_id)| {
                let selector = if !repair && index == 0 {
                    ".orbit/resources/activities/task_pilot.yaml"
                } else {
                    "file:src/remote.rs"
                };
                json!({
                    "task_id": task_id,
                    "context_files_before": [],
                    "context_files_after": [selector],
                    "disposition": "selectors",
                    "recommended_crew": "system",
                    "recommended_complexity": "medium",
                    "assessment_rationale": "The scripted repair changes one known source file.",
                    "confidence": "high",
                    "evidence_gaps": [],
                    "validation_approach": "Run the focused source test.",
                    "reassessment_triggers": ["the source revision changes"],
                    "blocked_by": [],
                    "duplicate_of": null,
                    "already_landed": null,
                    "release_action_required": null,
                    "adr_conflicts": [],
                    "utility_warnings": [],
                    "surface_warnings": [],
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "partition_index": input["partition_index"],
            "task_ids": task_ids,
            "tasks": tasks,
            "summary": "scripted pilot",
        }))
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        <OrbitRuntime as RuntimeHost>::resolve_cli_executor(self.runtime, provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        <OrbitRuntime as RuntimeHost>::tool_context_for_activity(
            self.runtime,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
}

#[test]
fn shipped_pipeline_repairs_only_invalid_task_and_preserves_partial_progress() {
    let fixture = task_pilot_job_fixture("agent-main", "agent-main");
    fs::write(
        fixture
            .global_root
            .join("resources/activities/task_pilot.yaml"),
        r#"schemaVersion: 2
kind: Activity
metadata:
  name: task_pilot
spec:
  type: deterministic
  description: Scripted task-pilot fixture.
  input_schema_json: {type: object}
  output_schema_json: {type: object}
  action: scripted_task_pilot
  config: {}
"#,
    )
    .expect("write scripted pilot activity");
    let task_ids = (0..2)
        .map(|index| {
            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: format!("pipeline repair {index}"),
                    description: "pipeline repair fixture".to_string(),
                    acceptance_criteria: vec!["repair completes".to_string()],
                    plan: "run pilot".to_string(),
                    priority: TaskPriority::Medium,
                    task_type: Some(TaskType::Bug),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .expect("seed task")
                .id
        })
        .collect::<Vec<_>>();
    let host = ScriptedPilotHost {
        runtime: &fixture.runtime,
        calls: Mutex::new(Vec::new()),
    };

    let outcome = try_execute_named_job(
        &fixture.runtime,
        &fixture.repo_root,
        &host,
        "task_pilot_pipeline",
        json!({"task_ids": task_ids, "base_branch": "agent-main"}),
        "jrun-task-pilot-repair",
    )
    .expect("execute task-pilot repair workflow");

    assert!(outcome.success, "{outcome:?}");
    assert_eq!(outcome.pipeline["apply"]["applied_count"], 1);
    assert_eq!(outcome.pipeline["apply"]["repair_count"], 1);
    assert_eq!(outcome.pipeline["apply_repairs"]["applied_count"], 2);
    assert_eq!(host.calls.lock().unwrap().as_slice(), &[false, true]);
    for task_id in task_ids {
        let task = fixture.runtime.get_task(&task_id).unwrap();
        assert_eq!(task.context_files, vec!["file:src/remote.rs"]);
        assert_eq!(
            task.complexity,
            Some(orbit_types::task::TaskComplexity::Medium)
        );
    }
}

#[cfg(unix)]
fn task_pilot_provider_stdout(task_id: &str) -> String {
    let response = json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {
            "partition_index": 0,
            "task_ids": [task_id],
            "tasks": [{
                "task_id": task_id,
                "context_files_before": [],
                "context_files_after": ["file:src/remote.rs"],
                "disposition": "selectors",
                "recommended_crew": "system",
                "recommended_complexity": "medium",
                "assessment_rationale": "The real CLI fixture identified one existing source file.",
                "confidence": "high",
                "evidence_gaps": [],
                "validation_approach": "Assert the worker terminal audit trail.",
                "reassessment_triggers": ["the source revision changes"],
                "blocked_by": [],
                "duplicate_of": null,
                "already_landed": null,
                "release_action_required": null,
                "adr_conflicts": [],
                "utility_warnings": [],
                "surface_warnings": [],
            }],
            "summary": "real CLI fixture",
        },
        "error": null,
    });
    [
        json!({"type": "thread.started", "thread_id": "task-pilot-fixture"}).to_string(),
        json!({
            "type": "item.completed",
            "item": {
                "id": "answer",
                "type": "agent_message",
                "text": response.to_string(),
            },
        })
        .to_string(),
        json!({
            "type": "turn.completed",
            "usage": {"input_tokens": 17, "cached_input_tokens": 0, "output_tokens": 5},
        })
        .to_string(),
    ]
    .join("\n")
        + "\n"
}

#[cfg(unix)]
fn install_store_replacing_provider(fixture: &TaskPilotJobFixture, stdout: &str) {
    use std::os::unix::fs::PermissionsExt;

    let fake_provider = fixture.repo_root.join("claude");
    fs::write(fake_provider.with_extension("stdout"), stdout).expect("write provider stdout");
    let database = fixture.global_root.join("orbit.db");
    fs::write(
        &fake_provider,
        format!(
            "#!/bin/sh\n\
             cat >/dev/null\n\
             database='{}'\n\
             cp \"$database\" \"$database.rebound\"\n\
             cp \"$database-wal\" \"$database.rebound-wal\"\n\
             cp \"$database-shm\" \"$database.rebound-shm\"\n\
             mv \"$database\" \"$database.retired\"\n\
             mv \"$database-wal\" \"$database.retired-wal\"\n\
             mv \"$database-shm\" \"$database.retired-shm\"\n\
             mv \"$database.rebound\" \"$database\"\n\
             mv \"$database.rebound-wal\" \"$database-wal\"\n\
             mv \"$database.rebound-shm\" \"$database-shm\"\n\
             cat \"$0.stdout\"\n",
            database.display(),
        ),
    )
    .expect("write provider executable");
    let mut permissions = fs::metadata(&fake_provider)
        .expect("provider metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_provider, permissions).expect("make provider executable");

    let now = Utc::now();
    fixture
        .runtime
        .upsert_executor_def(&ExecutorDef {
            name: "claude".to_string(),
            executor_type: ExecutorType::DirectAgent,
            command: Some(fake_provider.display().to_string()),
            args: Vec::new(),
            stdout_format: None,
            model_pair_override: None,
            model_flag: None,
            timeout_seconds: None,
            env: HashMap::new(),
            sandbox: None,
            allow_fallback: false,
            created_at: Some(now),
            updated_at: Some(now),
        })
        .expect("seed real CLI provider");
}

#[cfg(unix)]
#[test]
fn real_cli_task_pilot_worker_persists_apply_and_terminal_completion_audit() {
    let fixture = task_pilot_job_fixture("agent-main", "agent-main");
    let task = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Real CLI task-pilot completion fixture".to_string(),
            description: "Exercise provider completion through the detached worker path."
                .to_string(),
            acceptance_criteria: vec!["pilot result is applied durably".to_string()],
            priority: TaskPriority::High,
            task_type: Some(TaskType::Bug),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .expect("seed task-pilot target");
    let stdout = task_pilot_provider_stdout(&task.id);
    install_store_replacing_provider(&fixture, &stdout);

    let input = json!({"task_ids": [task.id], "base_branch": "agent-main"});
    let run = fixture
        .runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .expect("insert task-pilot run");
    fixture
        .runtime
        .seed_v2_pipeline_run(&run, &input, None, JobRunTrigger::cli())
        .expect("seed task-pilot run state");
    fixture
        .runtime
        .execute_pipeline_run_worker(&run.run_id)
        .expect("real CLI task-pilot worker succeeds");

    // Reopen from the authoritative paths instead of trusting the worker's
    // cached handles. Without the provider-return rebind, the old connection
    // can report a self-consistent success that no post-exit observer sees.
    let reopened =
        OrbitRuntime::from_roots(&fixture.global_root, &fixture.repo_root.join(".orbit"))
            .expect("reopen authoritative runtime");
    let stored = reopened.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Success);
    assert!(!stored.steps.is_empty(), "terminal steps must be durable");
    let state = reopened
        .read_run_state(&run.run_id)
        .expect("read completed pipeline state")
        .expect("completed pipeline state exists");
    let applied = reopened.get_task(&task.id).expect("read applied task");
    assert_eq!(
        applied.context_files,
        vec!["file:src/remote.rs"],
        "pipeline: {}",
        state.pipeline
    );
    assert!(
        reopened
            .get_task_history(&task.id)
            .expect("read task history")
            .iter()
            .any(|entry| entry.event == "task_pilot_applied"),
        "successful provider output must cross the real apply boundary"
    );

    let mut events = reopened
        .list_v2_audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            run_id: Some(run.run_id.clone()),
            ..Default::default()
        })
        .expect("read terminal audit trail");
    events.sort_by_key(|event| event.ts);
    let event_types = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    let provider_finished = event_types
        .iter()
        .position(|event| *event == "cli.invocation.finished")
        .expect("provider-finished audit");
    let activity_finished = event_types[provider_finished + 1..]
        .iter()
        .position(|event| *event == "activity.finished")
        .map(|offset| provider_finished + 1 + offset)
        .expect("activity-finished audit after provider exit");
    let step_finished = event_types[activity_finished + 1..]
        .iter()
        .position(|event| *event == "step.finished")
        .map(|offset| activity_finished + 1 + offset)
        .expect("step-finished audit after activity completion");
    assert!(
        event_types[step_finished + 1..].contains(&"run.finished"),
        "run-finished audit must follow the provider/activity/step completion chain"
    );

    let finished: Value = serde_json::from_str(&events[provider_finished].payload_json)
        .expect("parse provider-finished audit");
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["timed_out"], false);
    let stdout_ref = finished["stdout_blob_ref"]
        .as_str()
        .expect("stdout blob reference");
    let stderr_ref = finished["stderr_blob_ref"]
        .as_str()
        .expect("stderr blob reference");
    let blobs = fixture.runtime.paths().audit_dir.join("blobs");
    assert_eq!(
        fs::read(blobs.join(&stdout_ref[..2]).join(stdout_ref)).expect("read stdout blob"),
        stdout.as_bytes()
    );
    assert_eq!(
        fs::read(blobs.join(&stderr_ref[..2]).join(stderr_ref)).expect("read stderr blob"),
        b""
    );
}
