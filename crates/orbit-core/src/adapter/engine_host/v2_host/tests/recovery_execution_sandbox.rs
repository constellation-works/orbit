//! Native Linux execution coverage for the generic step-failure recovery leaf.
//!
//! The outer test process owns no mutable Orbit fixture state. It starts one
//! exact child after clearing inherited managed-run authority; that child uses
//! disposable roots and launches a fake local Codex provider through the real
//! Bubblewrap executable selected by Orbit.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use orbit_engine::activity_job::{V2ActivityCatalog, load_job_asset};
use orbit_engine::{DispatchError, RuntimeHost, V2AuditWriter, execute_job_with_resume};
use orbit_store::InvocationQuery;
use orbit_types::workflow::activity_job::{ActivityV2Spec, DeterministicSpec, JobV2StepBody};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::Value;

use super::super::test_support::{runtime_with_workspace_config, seed_executor};
use super::super::*;

const CHILD_ENV: &str = "ORBIT_TEST_REAL_STEP_RECOVERY_CHILD";
const EXACT_TEST: &str = "adapter::engine_host::v2_host::tests::recovery_execution_sandbox::step_failure_recovery_runs_a_real_linux_sandboxed_subprocess";
const OPERATOR_COMMAND: &str = "cargo test -p orbit-core --lib adapter::engine_host::v2_host::tests::recovery_execution_sandbox::step_failure_recovery_runs_a_real_linux_sandboxed_subprocess -- --ignored --exact --nocapture";

#[test]
#[ignore = "requires native Linux Bubblewrap; run the owning-host command from the Linux sandbox runbook"]
fn step_failure_recovery_runs_a_real_linux_sandboxed_subprocess() {
    if std::env::var_os(CHILD_ENV).is_none() {
        let binary = std::env::current_exe().expect("current test binary");
        let scratch = tempfile::Builder::new()
            .prefix("orbit-native-recovery-")
            .tempdir_in("/var/tmp")
            .expect("create disk-backed native recovery scratch outside /tmp");
        let mut command = Command::new(&binary);
        command.args(["--ignored", "--exact", EXACT_TEST, "--nocapture"]);
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let output = command
            .env(CHILD_ENV, "1")
            .env("TMPDIR", scratch.path())
            .output()
            .expect("run isolated Linux recovery child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "native Linux recovery check failed or was NOT RUN; binary={}; command=`{OPERATOR_COMMAND}`\n{stdout}\n{stderr}",
            binary.display()
        );
        assert!(
            stdout
                .lines()
                .any(|line| line.starts_with("test result: ok. 1 passed;")),
            "isolated child did not run exactly one test: {stdout}"
        );
        return;
    }

    let binary = std::env::current_exe().expect("current test binary");
    let probe = orbit_exec::probe_bwrap();
    assert!(
        probe.available,
        "native Linux recovery check NOT RUN: {}; binary={}; operator command=`{OPERATOR_COMMAND}`",
        probe.detail,
        binary.display()
    );

    for (failed_step_id, resume_implement) in [("implement_one", false), ("commit", true)] {
        run_failure_shape(failed_step_id, resume_implement, &probe.trusted_path);
    }
}

fn run_failure_shape(failed_step_id: &'static str, resume_implement: bool, trusted_bwrap: &str) {
    let (_root, runtime, primary) = runtime_with_workspace_config(Some(
        r#"
[workflow]
default_crew = "repair"
system_crew = "repair"
[crews.repair]
provider = "codex"
model = "gpt-6-luna"
"#,
    ));
    seed_executor(
        &runtime,
        "codex",
        Some(orbit_types::workflow::ExecutorSandboxKind::LinuxBwrap),
    );
    git(&primary, &["init"]);
    fs::write(primary.join(".gitignore"), ".orbit/\n")
        .expect("ignore disposable Orbit runtime state");
    git(&primary, &["add", ".gitignore"]);
    git(
        &primary,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    let base_sha = git_stdout(&primary, &["rev-parse", "HEAD"]);
    let assigned = runtime
        .paths()
        .orbit_dir
        .join(format!("state/worktrees/orbit-{failed_step_id}-fixture"));
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            &format!("candidate-{failed_step_id}"),
            assigned.to_str().expect("UTF-8 worktree path"),
        ],
    );
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: format!("{failed_step_id} recovery boundary"),
            description: "Retain the managed assignment.".to_string(),
            plan: "Exercise native recovery execution.".to_string(),
            ..Default::default()
        })
        .expect("add isolated fixture task");
    let run_id = format!("run-{failed_step_id}-boundary");
    let dotenv = assigned.join(".env");
    let primary_guard = primary.join("primary-protected.txt");
    fs::write(&dotenv, "fixture-secret\n").expect("write fixture dotenv");
    fs::write(&primary_guard, "primary-intact\n").expect("write primary guard");
    let git_pointer = fs::read(assigned.join(".git")).expect("read worktree git pointer");
    let provider_dir = assigned.join(format!("fixture-bin-{failed_step_id}"));
    fs::create_dir(&provider_dir).expect("create fake provider directory");
    let provider = provider_dir.join("codex");
    let observation = assigned.join(format!("recovery-ran-{failed_step_id}.txt"));
    write_fake_provider(
        &provider,
        &observation,
        &assigned,
        &primary_guard,
        &task.id,
        &run_id,
    );
    let provider_string = provider.display().to_string();
    let _provider_override =
        orbit_common::test_env::scoped([("ORBIT_V2_CLI_CODEX", Some(provider_string.as_str()))]);

    assert_missing_managed_context_fails_closed(&runtime, &primary);
    let mut job = load_job_asset(include_str!(
        "../../../../../assets/jobs/task_pr_pipeline.yaml"
    ))
    .expect("load task PR pipeline")
    .spec;
    let mut catalog = V2ActivityCatalog::new();
    catalog
        .load_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/activities"))
        .expect("load activity catalog");
    orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog)
        .expect("resolve pipeline activities");
    replace_failed_target(&mut job, failed_step_id);
    job.failure_activity = None;
    job.resolved_failure_activity = None;

    let input = serde_json::json!({
        "task_ids": [task.id],
        "allowed_crews": ["repair"],
    });
    let mut resume = PipelineState::new(
        run_id.clone(),
        "task_pr_pipeline".to_string(),
        input.clone(),
    );
    resume.step_states.insert(0, JobRunState::Success);
    resume.step_outputs.insert(
        0,
        serde_json::json!({
            "job_run_id": run_id,
            "workspace_path": assigned,
            "base_ref": "refs/heads/agent-main",
            "base_sha": base_sha,
        }),
    );
    if resume_implement {
        resume.step_states.insert(1, JobRunState::Success);
        resume.step_outputs.insert(1, serde_json::json!({}));
    }
    let audit = V2AuditWriter::with_disk_sinks(
        &runtime.paths().orbit_dir.join("state/audit"),
        runtime.v2_audit_store().expect("audit store"),
        "fixture",
        &run_id,
        "test",
        Some(&primary),
    )
    .expect("audit writer");
    let host = RecoveryExecutionHost {
        runtime: &runtime,
        assigned: &assigned,
        task_id: &task.id,
        failed_step_id,
        deterministic_calls: AtomicUsize::new(0),
    };
    let primary_status_before = git_output(&primary, &["status", "--porcelain"]);

    let error = execute_job_with_resume(&job, input, &run_id, audit.clone(), &host, Some(&resume))
        .expect_err("post-recovery retry remains deterministically failed");
    let recovery_events = audit.events_snapshot().expect("recovery audit snapshot");
    let recovery = recovery_events
        .iter()
        .find(|event| event.envelope.event_type == "step.recovery_attempted")
        .map(|event| serde_json::to_value(event).expect("serialize recovery event"))
        .expect("durable recovery attempt");
    assert_eq!(
        recovery["recovery_succeeded"],
        true,
        "native recovery dispatch failed before the bounded post-recovery attempt: {recovery}; events={}",
        serde_json::to_string(&recovery_events).expect("serialize recovery audit")
    );
    let message = error.to_string();
    assert!(
        message.contains(&format!("original {failed_step_id} failure"))
            && message.contains("post-recovery attempt error"),
        "original and post-recovery outcomes must stay distinct: {error:?}"
    );
    assert_eq!(
        host.deterministic_calls.load(Ordering::SeqCst),
        2,
        "one original failure and one bounded post-recovery attempt"
    );
    assert_eq!(
        fs::read_to_string(&observation).expect("provider observation"),
        format!("{task_id}|{run_id}|{failed_step_id}\n", task_id = task.id),
        "the fake provider must execute in the assigned sandbox with preserved identity"
    );
    assert_eq!(
        fs::read_to_string(&dotenv).expect("dotenv"),
        "fixture-secret\n"
    );
    assert_eq!(
        fs::read_to_string(&primary_guard).expect("primary guard"),
        "primary-intact\n"
    );
    assert_eq!(
        fs::read(assigned.join(".git")).expect("git pointer after recovery"),
        git_pointer,
        "provider must not mutate linked-worktree Git metadata"
    );
    assert_eq!(
        git_output(&primary, &["status", "--porcelain"]),
        primary_status_before,
        "recovery execution must leave the primary checkout unchanged"
    );

    assert_recovery_evidence(
        &runtime,
        &audit,
        failed_step_id,
        &task.id,
        &run_id,
        trusted_bwrap,
    );
}

fn replace_failed_target(job: &mut orbit_types::workflow::activity_job::JobV2, step_id: &str) {
    let failed_step = if step_id == "implement_one" {
        let implement_bundle = job
            .steps
            .iter_mut()
            .find(|step| step.id == "implement_bundle")
            .expect("implementation loop");
        let JobV2StepBody::Loop { loop_ } = &mut implement_bundle.body else {
            panic!("resolved implementation loop")
        };
        loop_
            .steps
            .iter_mut()
            .find(|step| step.id == step_id)
            .expect("implementation target")
    } else {
        job.steps
            .iter_mut()
            .find(|step| step.id == step_id)
            .expect("commit target")
    };
    let JobV2StepBody::Target(target) = &mut failed_step.body else {
        panic!("resolved failed target")
    };
    target.spec = ActivityV2Spec::Deterministic(DeterministicSpec {
        action: format!("{step_id}_fixture"),
        config: Value::Null,
    });
}

fn write_fake_provider(
    provider: &Path,
    observation: &Path,
    assigned: &Path,
    primary_guard: &Path,
    task_id: &str,
    run_id: &str,
) {
    for value in [
        provider,
        observation,
        assigned,
        primary_guard,
        Path::new(task_id),
        Path::new(run_id),
    ] {
        assert!(!value.as_os_str().to_string_lossy().contains('\''));
    }
    let script = format!(
        r#"#!/bin/sh
set -u
payload=$(cat)
test "$(pwd -P)" = '{assigned}' || exit 41
test "$ORBIT_MANAGED_RUN_CONTEXT" = 1 || exit 42
test "$ORBIT_RUN_ID" = '{run_id}' || exit 43
test "$ORBIT_TASK_ID" = '{task_id}' || exit 44
printf '%s' "$payload" | grep -F '"run_id":"{run_id}"' >/dev/null || exit 45
printf '%s' "$payload" | grep -F '"id":"{task_id}"' >/dev/null || exit 46
test "$(cat '{primary_guard}')" = 'primary-intact' || exit 51
if printf 'changed\n' 2>/dev/null > '{dotenv}'; then exit 47; fi
if printf 'changed\n' 2>/dev/null > '{primary_guard}'; then exit 48; fi
if printf 'changed\n' 2>/dev/null > '{git_pointer}'; then exit 49; fi
printf '%s\n' '{task_id}|{run_id}|{failed_step_id}' > '{observation}' || exit 50
printf '%s\n' '{{"schemaVersion":1,"status":"success","result":{{"recovered":true}},"error":null}}'
"#,
        assigned = assigned.display(),
        run_id = run_id,
        task_id = task_id,
        dotenv = assigned.join(".env").display(),
        primary_guard = primary_guard.display(),
        git_pointer = assigned.join(".git").display(),
        failed_step_id = run_id
            .strip_prefix("run-")
            .and_then(|value| value.strip_suffix("-boundary"))
            .expect("fixture run shape"),
        observation = observation.display(),
    );
    fs::write(provider, script).expect("write fake provider");
    fs::set_permissions(provider, fs::Permissions::from_mode(0o755))
        .expect("make fake provider executable");
}

fn assert_missing_managed_context_fails_closed(runtime: &OrbitRuntime, primary: &Path) {
    let direct = runtime
        .resolve_executor_sandbox("codex", Some("implementer"), Some(primary))
        .expect("resolve direct sandbox")
        .expect("Linux sandbox descriptor");
    assert!(!direct.managed_worktree);
    let error = orbit_exec::compile_linux_bwrap_argv(
        &direct.fs_profile,
        "/bin/true",
        &[],
        Some(primary),
        direct.managed_worktree,
    )
    .expect_err("missing managed worktree context must fail closed");
    assert!(error.to_string().contains("non-subtree denyModify"));
}

fn assert_recovery_evidence(
    runtime: &OrbitRuntime,
    audit: &V2AuditWriter,
    step_id: &str,
    task_id: &str,
    run_id: &str,
    trusted_bwrap: &str,
) {
    let events = audit.events_snapshot().expect("audit events");
    for event_type in [
        "cli.invocation.started",
        "cli.invocation.process",
        "cli.invocation.finished",
        "step.recovery_attempted",
        "step.post_recovery_attempt",
    ] {
        assert_eq!(
            events
                .iter()
                .filter(|event| event.envelope.event_type == event_type)
                .count(),
            1,
            "bounded recovery must persist exactly one `{event_type}` event"
        );
    }
    let event = |kind: &str| {
        events
            .iter()
            .find(|event| event.envelope.event_type == kind)
            .map(|event| serde_json::to_value(event).expect("serialize audit event"))
            .expect("required audit event")
    };
    let started = event("cli.invocation.started");
    assert_eq!(
        started["cwd"],
        runtime
            .paths()
            .orbit_dir
            .join(format!("state/worktrees/orbit-{step_id}-fixture"))
            .display()
            .to_string()
    );
    assert_eq!(started["sandbox_backend"], "linux-bwrap");
    assert_eq!(started["sandbox_trusted_wrapper"], trusted_bwrap);
    assert_eq!(started["sandbox_write_enforcement"], "write_enforced");
    assert_eq!(started["argv_redacted"][0], trusted_bwrap);
    let process = event("cli.invocation.process");
    assert!(process["pid"].as_u64().is_some_and(|pid| pid > 0));
    let finished = event("cli.invocation.finished");
    assert_eq!(finished["exit_code"], 0);
    assert_eq!(finished["timed_out"], false);
    let recovery = event("step.recovery_attempted");
    assert_eq!(recovery["step_id"], step_id);
    assert_eq!(recovery["recovery_activity"], "step_failure_recovery");
    assert_eq!(recovery["recovery_succeeded"], true);
    assert!(recovery.get("failure_phase").is_none());
    let post = event("step.post_recovery_attempt");
    assert_eq!(post["step_id"], step_id);
    assert_eq!(post["outcome"], "error");
    assert!(
        post["error_message"]
            .as_str()
            .is_some_and(|message| message.contains(&format!("original {step_id} failure")))
    );

    let records = runtime
        .invocation_records(InvocationQuery {
            job_run_id: Some(run_id.to_string()),
            limit: 10,
            ..InvocationQuery::default()
        })
        .expect("recovery invocation records");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].activity_id, "step_failure_recovery");
    assert_eq!(records[0].agent, "codex");
    assert_eq!(records[0].model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(records[0].task_ids, [task_id.to_string()]);
}

fn git(repo: &Path, args: &[&str]) {
    let output = git_output(repo, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = git_output(repo, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_string()
}

fn git_output(repo: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git fixture command")
}

struct RecoveryExecutionHost<'a> {
    runtime: &'a OrbitRuntime,
    assigned: &'a Path,
    task_id: &'a str,
    failed_step_id: &'static str,
    deterministic_calls: AtomicUsize,
}

impl RuntimeHost for RecoveryExecutionHost<'_> {
    fn run_deterministic(
        &self,
        action: &str,
        _: &Value,
        _: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        assert_eq!(action, format!("{}_fixture", self.failed_step_id));
        self.deterministic_calls.fetch_add(1, Ordering::SeqCst);
        Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("original {} failure", self.failed_step_id),
        })
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        self.runtime.system_crew_for_dispatch()
    }

    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<orbit_engine::CrewConfig>, DispatchError> {
        assert_eq!(input["crew"], "repair");
        assert_eq!(input["allowed_crews"], serde_json::json!(["repair"]));
        self.runtime.agent_crew_config_for_input(input)
    }

    fn resolve_cli_executor(
        &self,
        provider: &str,
    ) -> Result<orbit_engine::ResolvedCliExecutor, DispatchError> {
        assert_eq!(provider, "codex");
        self.runtime.resolve_cli_executor(provider)
    }

    fn resolve_activity_tools(
        &self,
        ids: &[String],
        baseline: &[String],
    ) -> Result<orbit_engine::ResolvedActivityTools, DispatchError> {
        assert_eq!(ids, &[self.task_id.to_string()]);
        self.runtime.resolve_activity_tools(ids, baseline)
    }

    fn task_context_for_agent_input(&self, input: &Value) -> Result<Option<Value>, DispatchError> {
        assert_eq!(input["failed_step_id"], self.failed_step_id);
        assert_eq!(
            input["run_id"],
            format!("run-{}-boundary", self.failed_step_id)
        );
        assert_eq!(input["workspace_path"], self.assigned.display().to_string());
        assert_eq!(input["repo_root"], input["workspace_path"]);
        assert_eq!(
            input["failed_step_input"]["workspace_path"],
            input["workspace_path"]
        );
        let context = self.runtime.task_context_for_agent_input(input)?;
        assert_eq!(context.as_ref().expect("task context")["id"], self.task_id);
        Ok(context)
    }

    fn tool_context_for_activity(
        &self,
        run: Option<&str>,
        profile: Option<&str>,
        audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        self.runtime
            .tool_context_for_activity(run, profile, audit, programs)
    }

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        profile: Option<&str>,
        cwd: Option<&Path>,
    ) -> Result<Option<orbit_engine::ResolvedSandbox>, DispatchError> {
        assert_eq!(cwd, Some(self.assigned));
        let sandbox = self
            .runtime
            .resolve_executor_sandbox(provider, profile, cwd)?;
        let resolved = sandbox.as_ref().expect("Linux sandbox");
        assert!(resolved.managed_worktree);
        assert!(
            resolved
                .fs_profile
                .modify
                .iter()
                .any(|rule| rule.starts_with('!') && rule.contains("/**/.env")),
            "managed recovery must retain default dotenv denyModify rules"
        );
        Ok(sandbox)
    }

    fn agent_provider_config(&self) -> std::collections::HashMap<String, String> {
        self.runtime.agent_provider_config()
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        self.runtime.agent_subprocess_environment(required_env_vars)
    }

    fn orbit_registry_root(&self) -> Option<String> {
        self.runtime.orbit_registry_root()
    }

    fn orbit_workspace_selector(&self) -> Option<String> {
        self.runtime.orbit_workspace_selector()
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), orbit_common::OrbitError> {
        self.runtime.refresh_persistence_after_cli_provider()
    }

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        input: &Value,
        trace: &orbit_types::telemetry::InvocationTrace,
    ) -> Result<(), DispatchError> {
        self.runtime.persist_invocation_trace(
            job_run_id,
            activity_id,
            provider,
            model,
            input,
            trace,
        )
    }
}
