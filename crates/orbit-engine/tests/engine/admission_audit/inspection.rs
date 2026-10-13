use super::cli_support::{TestHost, persisted_writer, write_executable};
use orbit_engine::{DispatchError, execute_job_with_resume, load_activity_asset};
use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, BackoffStrategy, JobKind, JobV2, JobV2Step, JobV2StepBody, RetrySpec,
    TargetStep, V2AuditEventKind,
};
use serde_json::json;
use std::fs;
use tempfile::tempdir;
/// [ORB-14091] A task-pilot dispatch configured to retry still stops after
/// the first attempt when `inspection_revision` is a 7-character id. The
/// provider is not started.
#[test]
fn short_inspection_revision_fails_a_retried_task_pilot_once() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::short_inspection_revision_fails_a_retried_task_pilot_once"
    )) {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).expect("workspace");
    let calls = temp.path().join("provider-calls");
    let script = temp.path().join("claude");
    write_executable(
        &script,
        &format!(
            "#!/bin/sh\nprintf x >> '{}'\ncat > /dev/null\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
            calls.display()
        ),
    );
    let asset = load_activity_asset(include_str!(
        "../../../../orbit-core/assets/activities/task_pilot.yaml"
    ))
    .expect("task_pilot asset");
    assert_eq!(asset.name, "task_pilot");
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec.clone() else {
        panic!("task_pilot is an agent loop");
    };
    assert_eq!(asset.spec.fs_profile.as_deref(), Some("reviewer"));

    let job = JobV2 {
        state: JobScheduleState::Enabled,
        owns_task_worktree: false,
        task_delivery: None,
        default_input: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        failure_activity: None,
        resolved_failure_activity: None,
        final_recovery_activity: None,
        resolved_final_recovery_activity: None,
        max_active_runs: 1,
        kind: JobKind::Workflow,
        steps: vec![JobV2Step {
            id: "pilot".to_string(),
            when: None,
            retry: Some(RetrySpec {
                max_attempts: 3,
                initial_backoff_ms: 1,
                backoff_cap_ms: 1,
                backoff_strategy: BackoffStrategy::Linear,
            }),
            recovery_activity: None,
            resolved_recovery_activity: None,
            body: JobV2StepBody::Target(TargetStep {
                spec: ActivityV2Spec::AgentLoop(spec),
                activity_name: Some("task_pilot".to_string()),
                input_schema_json: Some(asset.spec.input_schema_json.clone()),
                fs_profile: asset.spec.fs_profile.clone(),
                default_input: None,
                timeout_seconds: 0,
                session: None,
            }),
        }],
    };
    let audit = persisted_writer(
        &temp.path().join("audit"),
        "job-short-inspection",
        "claude:test",
    );
    let host = TestHost::with_command(script.display().to_string());
    let error = execute_job_with_resume(
        &job,
        json!({
            "task_ids": ["ORB-1"],
            "workspace_path": workspace,
            "base_branch": "agent-main",
            "inspection_revision": "abcdef0",
            "partition_index": 0,
        }),
        "job-short-inspection",
        audit.clone(),
        &host,
        None,
    )
    .expect_err("a 7-character inspection revision fails the pilot");

    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("short inspection revision is permanent, got {error}");
    };
    assert!(
        message.contains("inspection_revision must be a full commit id"),
        "{message}"
    );
    assert!(
        error.is_non_retryable(),
        "ORB-14091: a short inspection revision must not be retried"
    );
    assert!(
        !calls.exists(),
        "the provider must not start for a rejected revision"
    );
    let events = audit.events_snapshot().expect("audit events");
    let starts = events
        .iter()
        .filter(|event| {
            matches!(
                &event.kind,
                V2AuditEventKind::ActivityStarted { activity_name, .. }
                    if activity_name == "pilot"
            )
        })
        .count();
    assert_eq!(starts, 1, "ORB-14091: one task-pilot attempt, got {starts}");
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind, V2AuditEventKind::StepRetry { .. })),
        "a permanent inspection failure emits no step retry"
    );
}

#[test]
fn validation_failures_are_permanent() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::validation_failures_are_permanent"
    )) {
        return;
    }
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let revision = super::orchestrator_env::init_repo(&repo);
    let other = "0123456789abcdef0123456789abcdef01234567";
    assert_ne!(revision, other);
    let revision = revision.as_str();
    let cases = [
        (
            json!({"inspection_revision": ""}),
            Some(repo.as_path()),
            Some("reviewer"),
            "pinned inspection_revision",
        ),
        (
            json!({"inspection_revision": 7}),
            Some(repo.as_path()),
            Some("reviewer"),
            "must be a commit id",
        ),
        (
            json!({"inspection_revision": "abcdef0"}),
            Some(repo.as_path()),
            Some("reviewer"),
            "full commit id",
        ),
        (
            json!({"inspection_revision": "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"}),
            Some(repo.as_path()),
            Some("reviewer"),
            "full commit id",
        ),
        (
            json!({"inspection_revision": other, "source_revision": revision}),
            None,
            Some("reviewer"),
            "differs from the prepared source_revision",
        ),
        (
            json!({"inspection_revision": revision}),
            Some(repo.as_path()),
            Some("implementer"),
            "reviewer filesystem profile",
        ),
        (
            json!({"inspection_revision": revision}),
            None,
            Some("reviewer"),
            "requires a workspace",
        ),
    ];
    for (input, source, profile, needle) in cases {
        assert_permanent(inspect_at_boundary(&input, source, profile), needle);
    }
}

/// Supply the same inspection inputs to the exported provider preparation boundary.
/// Every row refuses before the fixture provider can execute.
fn inspect_at_boundary(
    input: &serde_json::Value,
    source: Option<&std::path::Path>,
    profile: Option<&str>,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let mut input = input.clone();
    if let Some(source) = source {
        input["workspace_path"] = json!(source);
    }
    let audit = std::sync::Arc::new(orbit_engine::V2AuditWriter::new(
        "inspection-validation",
        "codex:test",
        std::sync::Arc::new(orbit_agent::loop_engine::audit::NullSink),
    ));
    orbit_engine::activity_job::cli_runner::run_cli_backend(
        &TestHost::with_command("/bin/true".into()),
        &super::cli_support::test_agent_loop_spec_for("codex", std::time::Duration::from_secs(5)),
        "inspection-validation",
        "inspection-validation",
        audit,
        &input,
        profile,
    )
}
fn assert_permanent(result: Result<orbit_engine::DispatchOutcome, DispatchError>, needle: &str) {
    let error = result.expect_err("deterministic inspection failure");
    let DispatchError::CliInvocationPermanent(message) = &error else {
        panic!("ORB-14091: expected a permanent inspection failure, got {error}");
    };
    assert!(
        error.is_non_retryable(),
        "permanent inspection failures skip the retry wrapper: {error}"
    );
    assert!(message.contains(needle), "{message}");
}

use orbit_common::process::shell::quote_posix_arg;
use orbit_common::test_env::{assert_child_test_passed, run_child_test};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
const STUB_GIT_DIR_ENV: &str = "ORBIT_TEST_INSPECTION_STUB_GIT_DIR";
const SLOW_COPY_CHILD: &str =
    "admission_audit::inspection::slow_full_ancestry_copy_outlasts_the_local_deadline";
const KILLED_LOOKUP_CHILD: &str =
    "admission_audit::inspection::a_commit_lookup_killed_by_a_signal_stays_retryable";
const KILL_VERIFY_MARKER: &str = "kill-verify";

#[test]
fn a_commit_lookup_killed_by_a_signal_stays_retryable() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::a_commit_lookup_killed_by_a_signal_stays_retryable"
    )) {
        return;
    }
    if let Some(stub_dir) = std::env::var_os(STUB_GIT_DIR_ENV) {
        let temp = tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let revision = super::orchestrator_env::init_repo(&repo);
        fs::write(Path::new(&stub_dir).join(KILL_VERIFY_MARKER), "").expect("kill switch");
        assert_retryable(
            inspect_at_boundary(
                &json!({"inspection_revision": revision, "source_revision": revision}),
                Some(&repo),
                Some("reviewer"),
            ),
            "rev-parse --verify",
        );
        return;
    }
    run_child_with_stub_git(KILLED_LOOKUP_CHILD);
}

#[test]
fn slow_full_ancestry_copy_outlasts_the_local_deadline() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::slow_full_ancestry_copy_outlasts_the_local_deadline"
    )) {
        return;
    }
    if let Some(stub_dir) = std::env::var_os(STUB_GIT_DIR_ENV) {
        fs::write(Path::new(&stub_dir).join("clocked-fetch"), "").expect("clocked copy");
        let temp = tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let revision = super::orchestrator_env::init_repo(&repo);
        let provider = temp.path().join("grok");
        write_executable(
            &provider,
            r#"#!/bin/sh
cat > /dev/null
head=$(git rev-parse HEAD) || exit 1
printf '{"schemaVersion":1,"status":"success","result":{"head":"%s"},"error":null}\n' "$head"
"#,
        );
        let audit = std::sync::Arc::new(orbit_engine::V2AuditWriter::new(
            "slow-inspection",
            "grok:test",
            std::sync::Arc::new(orbit_agent::loop_engine::audit::NullSink),
        ));
        use orbit_common::test_env::{
            SupervisorSeam, with_supervision_clock, with_supervisor_seam,
        };
        use std::{
            cell::Cell,
            rc::Rc,
            time::{Duration, Instant},
        };
        let origin = Instant::now();
        let now = Rc::new(Cell::new(origin));
        let clock = Rc::clone(&now);
        let ready = Path::new(&stub_dir).join("fetch-ready");
        let release = Path::new(&stub_dir).join("fetch-release");
        orbit_common::test_env::create_fixture_fifo(&release).expect("copy release FIFO");
        let seam = SupervisorSeam {
            wait: Some(Box::new(move |child| {
                if ready.exists() {
                    fs::remove_file(&ready).expect("consume copy readiness");
                    now.set(origin + Duration::from_secs(11));
                    // The copy remains blocked for this deadline decision. A
                    // local (10s) budget fails before the next poll releases it.
                    return Ok(None);
                }
                if now.get() != origin && release.exists() {
                    orbit_common::test_env::release_fixture_fifo(
                        &release,
                        Instant::now() + orbit_common::test_env::FIXTURE_STEP_DEADLINE,
                    )
                    .expect("release copy under its bulk deadline");
                    fs::remove_file(&release).expect("consume copy FIFO");
                }
                child.try_wait()
            })),
            ..SupervisorSeam::default()
        };
        let outcome = with_supervision_clock(move || clock.get(), || with_supervisor_seam(seam, ||
        orbit_engine::activity_job::cli_runner::run_cli_backend(
            &TestHost::with_command(provider.display().to_string()),
            &super::cli_support::test_agent_loop_spec_for("grok", std::time::Duration::from_secs(5)),
            "slow-inspection", "slow-inspection", audit,
            &json!({"inspection_revision": revision, "source_revision": revision, "workspace_path": repo}),
            Some("reviewer"),
        ))).expect("a copy under the bulk bound seeds the slot");
        assert!(
            outcome.success,
            "the inspection checkout can be read: {:?}",
            outcome.output
        );
        assert_eq!(outcome.output["head"], revision);
        return;
    }
    run_child_with_stub_git(SLOW_COPY_CHILD);
}

fn run_child_with_stub_git(test: &str) {
    let stub_dir = tempdir().expect("stub dir");
    set_fetch_delay(stub_dir.path(), 0);
    let delay = quote_posix_arg(&stub_dir.path().join("fetch-delay").display().to_string());
    let kill = quote_posix_arg(
        &stub_dir
            .path()
            .join(KILL_VERIFY_MARKER)
            .display()
            .to_string(),
    );
    let clocked = quote_posix_arg(&stub_dir.path().join("clocked-fetch").display().to_string());
    let ready = quote_posix_arg(&stub_dir.path().join("fetch-ready").display().to_string());
    let release = quote_posix_arg(&stub_dir.path().join("fetch-release").display().to_string());
    let git_path = quote_posix_arg(&real_git_path().display().to_string());
    write_executable(
        &stub_dir.path().join("git"),
        &format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" fetch \"*) if [ -e {clocked} ]; then : > {ready}; read go < {release}; else sleep \"$(cat {delay})\"; fi ;;\n  *\" --verify \"*) [ -e {kill} ] && kill -9 $$ ;;\nesac\nexec {git_path} \"$@\"\n"
        ),
    );
    let mut paths = vec![stub_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));

    let output_dir = tempdir().expect("child output dir");
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args(["--exact", test, "--nocapture"])
        .env(STUB_GIT_DIR_ENV, stub_dir.path())
        .env("PATH", std::env::join_paths(paths).expect("stub PATH"));
    let output = run_child_test(&mut command, test, output_dir.path());
    assert_child_test_passed(test, output.status, &output.stdout, &output.stderr);
}
fn set_fetch_delay(stub_dir: &Path, seconds: u64) {
    fs::write(stub_dir.join("fetch-delay"), seconds.to_string()).expect("fetch delay");
}
fn real_git_path() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .expect("git on PATH")
}
fn assert_retryable(result: Result<orbit_engine::DispatchOutcome, DispatchError>, needle: &str) {
    let error = result.expect_err("retryable inspection failure");
    let DispatchError::CliInvocationFailed(message) = &error else {
        panic!("ORB-14091: expected a retryable inspection failure, got {error}");
    };
    assert!(
        !error.is_non_retryable(),
        "slot exhaustion and git/IO stay retryable: {error}"
    );
    assert!(message.contains(needle), "{message}");
}
