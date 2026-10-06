#![allow(missing_docs)]

//! Retry classification for source-inspection failures [ORB-14091].
//!
//! Deterministic validation, slot ownership, a missing commit, and a
//! read-only checkout violation are permanent. Slot exhaustion and git or
//! filesystem IO stay retryable. The short-revision case is driven through
//! the job retry wrapper, which is the caller that decides whether a
//! task-pilot dispatch runs again.

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_types::workflow::JobScheduleState;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, BackoffStrategy, JobKind, JobV2, JobV2Step, JobV2StepBody, RetrySpec,
    TargetStep, V2AuditEventKind,
};
use serde_json::json;
use tempfile::tempdir;

use super::super::super::dispatcher::DispatchError;
use super::super::inspection::SourceInspection;
use super::test_support::{TestHost, persisted_writer, write_executable};
use crate::{execute_job_with_resume, load_activity_asset};

#[test]
fn validation_failures_are_permanent() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
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
        assert_permanent(
            SourceInspection::from_input(&input, source, profile),
            needle,
        );
    }
}

#[test]
fn missing_commit_and_slot_ownership_are_permanent() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": missing, "source_revision": missing}),
            Some(&repo),
            Some("reviewer"),
        ),
        "does not name a commit",
    );

    let blob = git(&repo, &["rev-parse", "HEAD:README"]);
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": blob}),
            Some(&repo),
            Some("reviewer"),
        ),
        "does not name a commit",
    );

    let held = SourceInspection::from_input(
        &json!({"inspection_revision": revision, "source_revision": revision}),
        Some(&repo),
        Some("reviewer"),
    )
    .expect("lease a slot")
    .expect("inspection checkout");
    let slot = held.root().parent().expect("slot dir").to_path_buf();
    drop(held);

    fs::write(slot.join("owner"), "not-orbit\n").expect("poison owner");
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "unrecognized owner",
    );

    fs::remove_file(slot.join("owner")).expect("remove owner");
    fs::create_dir_all(slot.join("checkout")).expect("unowned checkout");
    assert_permanent(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "unowned inspection checkout",
    );
}

#[test]
fn slot_exhaustion_and_git_io_stay_retryable() {
    let temp = tempdir().expect("tempdir");
    let bare = temp.path().join("bare");
    fs::create_dir(&bare).expect("bare dir");
    // A `.git` file naming a missing gitdir stops Git's upward discovery
    // here. Without it, a TMPDIR nested in another checkout resolves that
    // repository and the missing revision reads as a permanent missing SHA.
    fs::write(bare.join(".git"), "gitdir: missing-gitdir\n").expect("bare gitfile");
    let missing = "0123456789abcdef0123456789abcdef01234567";
    assert_retryable(
        SourceInspection::from_input(
            &json!({"inspection_revision": missing}),
            Some(&bare),
            Some("reviewer"),
        ),
        "not a git repository",
    );

    let repo = temp.path().join("repo");
    let revision = init_repo(&repo);
    fs::write(repo.join(".orbit"), "not a directory").expect("block the pool");
    assert_retryable(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": revision}),
            Some(&repo),
            Some("reviewer"),
        ),
        "File exists",
    );

    let pooled = temp.path().join("pooled");
    let pooled_revision = init_repo(&pooled);
    let input = json!({
        "inspection_revision": pooled_revision,
        "source_revision": pooled_revision,
    });
    let mut held = Vec::new();
    let mut exhausted = None;
    for _ in 0..64 {
        match SourceInspection::from_input(&input, Some(&pooled), Some("reviewer")) {
            Ok(Some(inspection)) => held.push(inspection),
            Ok(None) => panic!("a pinned revision must open an inspection"),
            Err(error) => {
                exhausted = Some(error);
                break;
            }
        }
    }
    assert!(!held.is_empty(), "the pool leased at least one slot");
    let error = exhausted.expect("the inspection pool is bounded");
    assert_retryable_error(&error, "all source inspection slots are leased");
}

/// [ORB-14091] A task-pilot dispatch configured to retry still stops after
/// the first attempt when `inspection_revision` is a 7-character id. The
/// provider is not started.
#[test]
fn short_inspection_revision_fails_a_retried_task_pilot_once() {
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
        "../../../../../orbit-core/assets/activities/task_pilot.yaml"
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

fn assert_permanent(result: Result<Option<SourceInspection>, DispatchError>, needle: &str) {
    match result {
        Err(error) => assert_permanent_error(&error, needle),
        Ok(_) => panic!("deterministic inspection failure"),
    }
}

fn assert_permanent_error(error: &DispatchError, needle: &str) {
    let DispatchError::CliInvocationPermanent(message) = error else {
        panic!("ORB-14091: expected a permanent inspection failure, got {error}");
    };
    assert!(
        error.is_non_retryable(),
        "permanent inspection failures skip the retry wrapper: {error}"
    );
    assert!(message.contains(needle), "{message}");
}

fn assert_retryable(result: Result<Option<SourceInspection>, DispatchError>, needle: &str) {
    match result {
        Err(error) => assert_retryable_error(&error, needle),
        Ok(_) => panic!("retryable inspection failure"),
    }
}

fn assert_retryable_error(error: &DispatchError, needle: &str) {
    let DispatchError::CliInvocationFailed(message) = error else {
        panic!("ORB-14091: expected a retryable inspection failure, got {error}");
    };
    assert!(
        !error.is_non_retryable(),
        "slot exhaustion and git/IO stay retryable: {error}"
    );
    assert!(message.contains(needle), "{message}");
}

fn init_repo(path: &Path) -> String {
    fs::create_dir_all(path).expect("repo dir");
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "inspection@example.test"]);
    git(path, &["config", "user.name", "inspection"]);
    fs::write(path.join("README"), "hello\n").expect("readme");
    git(path, &["add", "README"]);
    git(path, &["commit", "-m", "init"]);
    git(path, &["rev-parse", "HEAD"])
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
        ])
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout")
        .trim()
        .to_string()
}
