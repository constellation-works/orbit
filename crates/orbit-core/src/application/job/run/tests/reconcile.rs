//! Reconciliation and an in-flight cancellation over the same dead owner
//! (ORB-21876).
//!
//! A cancelled worker is reaped before its canceller reads the run again. Any
//! `orbit` process reconciling in that gap saw a dead owner and wrote
//! `interrupted`, so an operator's cancel blocked the task for recovery
//! instead of returning it to the backlog. The signal seam places that read
//! exactly in the gap, against real processes and with no timing involved.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use chrono::Utc;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRun, JobRunState, OPERATOR_STOPPED_ERROR_CODE, PipelineState};
use serde_json::json;

use super::super::WORKER_TERMINATED_ERROR_CODE;
use super::super::actions::{CANCELLATION_REQUEST_AUDIT, CancellationRequest};
use super::super::owner::signal_run_owner_process;
use super::super::root_cause::{RunFailureContextProfile, RunFailureRead};
use super::super::types::JobRunCancelOutcome;
use super::drain_cancel::in_progress_task;
use super::{insert_pending_run, test_runtime};
use crate::OrbitRuntime;
use crate::application::job::pipeline::WORKER_EXIT_AUDIT;
use crate::application::tests::run_isolated_test;

/// A process alone in its own group, as a pipeline worker runs.
fn spawn_process() -> Child {
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command.spawn().expect("spawn process")
}

/// `run` owned by `owner`, past the startup grace that shields a new owner.
fn mark_running(runtime: &OrbitRuntime, run: &JobRun, owner: &Child) -> JobRun {
    let jobs = runtime.stores().jobs();
    jobs.mark_job_run_running(
        &run.run_id,
        Utc::now() - chrono::Duration::seconds(30),
        owner.id(),
    )
    .expect("mark run running");
    jobs.get_job_run(&run.run_id)
        .expect("read run")
        .expect("run exists")
}

#[test]
fn reconciling_a_reaped_worker_mid_cancel_leaves_the_outcome_to_the_canceller() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciling_a_reaped_worker_mid_cancel_leaves_the_outcome_to_the_canceller,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    let owner = spawn_process();
    let owner_pid = owner.id();
    let running = mark_running(&runtime, &run, &owner);
    let task_id = in_progress_task(&runtime, &running);
    let mut bystander = spawn_process();
    // The worker's supervisor: reaping is what makes the owner read as gone.
    let supervisor = std::thread::spawn(move || {
        let mut owner = owner;
        owner.wait().expect("reap owner")
    });

    let result = runtime
        .cancel_job_run_with_options_and_signal(
            &run.run_id,
            CancellationRequest {
                actor: "dashboard",
                source: "web",
                reason: None,
                block_task: false,
                keep_candidate: false,
            },
            false,
            |target| {
                let outcome = signal_run_owner_process(target)?;
                supervisor.join().expect("supervisor thread");
                let observed = runtime.show_job_run(&target.run_id)?;
                assert_eq!(
                    observed.state,
                    JobRunState::Running,
                    "a reconciling reader must not finalize a run being cancelled"
                );
                Ok(outcome)
            },
        )
        .expect("cancel run");

    assert_eq!(result.outcome, JobRunCancelOutcome::Cancelled);
    assert_eq!(result.final_state, "cancelled");
    assert_eq!(
        result.signal_outcome.as_deref(),
        Some("terminated_process_group")
    );
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Cancelled
    );

    // The operator's outcome reaches the task: backlog, not a recovery block.
    assert_eq!(
        runtime.get_task(&task_id).expect("task").status,
        TaskStatus::Backlog
    );
    let history = runtime.get_task_history(&task_id).expect("task history");
    assert_eq!(
        history.last().expect("cancel history").event,
        "workflow_run_cancelled"
    );

    // Only the verified target was stopped.
    assert!(
        bystander.try_wait().expect("probe bystander").is_none(),
        "cancellation must not reach a process other than pid {owner_pid}"
    );
    bystander.kill().expect("end bystander");
    bystander.wait().expect("reap bystander");
}

/// A cancellation that can no longer finish must not hold the run: without
/// these exits a crashed canceller would strand a dead run as `running`.
#[test]
fn reconciliation_interrupts_a_dead_owner_once_its_cancellation_cannot_finish() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciliation_interrupts_a_dead_owner_once_its_cancellation_cannot_finish,
    )) {
        return;
    }

    enum Request {
        /// The canceller reported it could not stop the run.
        Failed,
        /// The canceller vanished long ago without reporting anything.
        Abandoned,
    }
    for (case, request) in [
        ("failed", Request::Failed),
        ("abandoned", Request::Abandoned),
    ] {
        let (_root, runtime) = test_runtime();
        let run = insert_pending_run(&runtime, "task_pr_pipeline");
        let mut owner = spawn_process();
        let running = mark_running(&runtime, &run, &owner);
        owner.kill().expect("end owner");
        owner.wait().expect("reap owner");

        let request_id = format!("cancel-{case}");
        let requested_at = match request {
            Request::Failed => Utc::now(),
            Request::Abandoned => Utc::now() - chrono::Duration::days(1),
        };
        runtime
            .record_pipeline_audit(
                CANCELLATION_REQUEST_AUDIT,
                Some(&running.run_id),
                Some("cli"),
                AuditEventStatus::Success,
                json!({
                    "request_id": request_id,
                    "run_id": running.run_id,
                    "requested_at": requested_at.to_rfc3339(),
                }),
                None,
            )
            .expect("record request");
        if matches!(request, Request::Failed) {
            runtime
                .record_cancellation_completion(
                    &running,
                    &request_id,
                    "failed",
                    None,
                    Some("stop unconfirmed"),
                )
                .expect("record failure");
        }

        let reconciled = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(reconciled.state, JobRunState::Interrupted, "{case}");
        assert_eq!(
            reconciled
                .steps
                .last()
                .and_then(|step| step.error_code.as_deref()),
            Some(WORKER_TERMINATED_ERROR_CODE),
            "{case}"
        );
    }
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git_stdout(repo, args))
        .trim()
        .to_string()
}

fn git_stdout(repo: &std::path::Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit@example.com")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_succeeds(repo: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Orbit Test")
        .env("GIT_AUTHOR_EMAIL", "orbit@example.com")
        .env("GIT_COMMITTER_NAME", "Orbit Test")
        .env("GIT_COMMITTER_EMAIL", "orbit@example.com")
        .status()
        .expect("git")
        .success()
}

/// A host-manager stop keeps the worker's uncommitted candidate under its
/// rescue ref before finalization, and the run record names that ref after
/// the worktree itself is removed.
#[test]
fn reconciliation_keeps_a_rescue_candidate_for_a_host_manager_stopped_worker() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciliation_keeps_a_rescue_candidate_for_a_host_manager_stopped_worker,
    )) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let repo = runtime.paths().repo_root.clone();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").expect("tracked file");
    git(&repo, &["add", "tracked.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);

    let run = insert_pending_run(&runtime, "task_pr_pipeline");
    let worktree = orbit_engine::run_worktree_paths(&repo, &run)
        .expect("worktree paths")
        .into_iter()
        .next()
        .expect("one worktree path");
    std::fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("worktree root");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("orbit/{}", run.run_id),
            worktree.to_str().expect("worktree path"),
        ],
    );
    std::fs::write(worktree.join("rescued.txt"), "kept\n").expect("uncommitted file");
    let head = git(&worktree, &["rev-parse", "HEAD"]);

    let mut state = orbit_types::workflow::PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        json!({}),
    );
    state.pipeline["worktree"] = json!({"workspace_path": worktree});
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("write run state");

    let mut owner = spawn_process();
    let running = mark_running(&runtime, &run, &owner);
    let pid = running.pid.expect("owner pid");
    owner.kill().expect("end owner");
    owner.wait().expect("reap owner");
    runtime
        .record_pipeline_audit(
            WORKER_EXIT_AUDIT,
            Some(&running.run_id),
            Some("supervisor"),
            AuditEventStatus::Success,
            json!({
                "worker_pid": pid,
                "exit_status": format!("signal: {}", libc::SIGTERM),
                "exit_code": null,
                "signal": libc::SIGTERM,
                "signal_name": "SIGTERM",
                "core_dumped": false,
                "scope": null,
                "signal_sender": {"pid": 1, "uid": 0, "command": "systemd"},
                "observed_at": Utc::now().to_rfc3339(),
            }),
            None,
        )
        .expect("record worker exit");

    let shown = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(shown.state, JobRunState::Held);
    let rescue_ref = format!("refs/orbit/rescue/{}", run.run_id);
    let message = shown
        .steps
        .last()
        .and_then(|step| step.error_message.as_deref())
        .unwrap_or("");
    assert!(
        message.contains(&format!("rescue candidate kept at `{rescue_ref}`")),
        "{message}"
    );
    let kept = runtime
        .read_run_state(&run.run_id)
        .expect("read state")
        .expect("state exists");
    let record = &kept.pipeline["rescue_candidate"];
    assert_eq!(record["rescue_ref"], rescue_ref.as_str(), "{record}");
    let commit = record["rescue_commit"].as_str().expect("rescue commit");
    assert_eq!(git(&repo, &["rev-parse", &rescue_ref]), commit);
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{commit}^")]),
        head,
        "the rescue commit sits on the candidate head"
    );
    assert_eq!(
        git(&repo, &["show", &format!("{commit}:rescued.txt")]),
        "kept"
    );

    // After the worktree is torn down, the ref still resolves and the
    // retained-candidate read names it instead of reporting manual salvage.
    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree.to_str().expect("worktree path"),
        ],
    );
    assert!(!worktree.exists());
    assert_eq!(git(&repo, &["rev-parse", &rescue_ref]), commit);
    let task = runtime
        .add_task(crate::application::task::TaskAddParams {
            title: "Stopped worker task".into(),
            ..Default::default()
        })
        .expect("add task");
    let retained = runtime.retained_candidate(&task, Some(&run.run_id), Some(&shown));
    assert!(
        retained.summary().contains(&rescue_ref),
        "{}",
        retained.summary()
    );
}

/// A dead owner whose recorded exit names a systemd-sent signal is a hold.
/// An unknown sender, a crash, a signal from `orbit`, and a systemd signal
/// after a memory-limit breach stay interrupted failures.
#[test]
fn reconciliation_holds_a_worker_the_host_manager_stopped() {
    if run_isolated_test(std::any::type_name_of_val(
        &reconciliation_holds_a_worker_the_host_manager_stopped,
    )) {
        return;
    }

    struct ExitCase {
        label: &'static str,
        signal: Option<i32>,
        exit_code: Option<i32>,
        command: Option<&'static str>,
        oom_kills: Option<u64>,
        held: bool,
    }
    let cases = [
        ExitCase {
            label: "systemd SIGTERM",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("systemd"),
            oom_kills: None,
            held: true,
        },
        ExitCase {
            label: "unknown sender",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: None,
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "crash exit",
            signal: None,
            exit_code: Some(1),
            command: None,
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "orbit SIGTERM",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("orbit"),
            oom_kills: None,
            held: false,
        },
        ExitCase {
            label: "systemd SIGTERM after oom",
            signal: Some(libc::SIGTERM),
            exit_code: None,
            command: Some("systemd"),
            oom_kills: Some(1),
            held: false,
        },
    ];

    for case in cases {
        let (_root, runtime) = test_runtime();
        let run = insert_pending_run(&runtime, "task_pr_pipeline");
        let mut owner = spawn_process();
        let running = mark_running(&runtime, &run, &owner);
        let pid = running.pid.expect("owner pid");
        owner.kill().expect("end owner");
        owner.wait().expect("reap owner");

        let signal_sender = case.command.map(|command| {
            json!({
                "pid": 1,
                "uid": 0,
                "command": command,
            })
        });
        let scope = case.oom_kills.map(|oom_kills| {
            json!({
                "unit": "orbit-worker-test.scope",
                "oom_kills": oom_kills,
                "tasks_refused": 0,
            })
        });
        runtime
            .record_pipeline_audit(
                WORKER_EXIT_AUDIT,
                Some(&running.run_id),
                Some("supervisor"),
                AuditEventStatus::Success,
                json!({
                    "worker_pid": pid,
                    "exit_status": case.signal.map(|signal| format!("signal: {signal}"))
                        .unwrap_or_else(|| "exit status: 1".to_string()),
                    "exit_code": case.exit_code,
                    "signal": case.signal,
                    "signal_name": case.signal.map(|_| "SIGTERM"),
                    "core_dumped": false,
                    "scope": scope,
                    "signal_sender": signal_sender,
                    "observed_at": Utc::now().to_rfc3339(),
                }),
                None,
            )
            .expect("record worker exit");

        let reconciled = runtime.show_job_run(&run.run_id).expect("show run");
        let code = reconciled
            .steps
            .last()
            .and_then(|step| step.error_code.as_deref());
        let message = reconciled
            .steps
            .last()
            .and_then(|step| step.error_message.as_deref())
            .unwrap_or("");
        let context = runtime
            .run_failure_context(
                &reconciled,
                None,
                RunFailureContextProfile::AllFailedLeaves(RunFailureRead::Observe),
                &mut |_| {},
            )
            .expect("failure query");
        let counted = context
            .causes
            .iter()
            .any(|cause| cause.run_id == reconciled.run_id);
        if case.held {
            assert_eq!(reconciled.state, JobRunState::Held, "{}", case.label);
            assert_eq!(code, Some(OPERATOR_STOPPED_ERROR_CODE), "{}", case.label);
            assert!(
                message.contains("marked held because the host manager stopped its worker"),
                "{}: {message}",
                case.label
            );
            assert!(
                !counted,
                "{} must be absent from the failure query",
                case.label
            );
        } else {
            assert_eq!(reconciled.state, JobRunState::Interrupted, "{}", case.label);
            assert_eq!(code, Some(WORKER_TERMINATED_ERROR_CODE), "{}", case.label);
            assert!(
                message.contains("marked interrupted"),
                "{}: {message}",
                case.label
            );
            assert!(counted, "{} must count as a failure", case.label);
        }
    }
}

/// What preservation must not change: the branch, both distinct file versions,
/// and the status that names them.
struct CandidateFingerprint {
    head: String,
    branch: String,
    status: Vec<u8>,
    staged_diff: Vec<u8>,
    working_diff: Vec<u8>,
    index_file: Vec<u8>,
    worktree_file: Vec<u8>,
    commits: String,
}

fn candidate_fingerprint(repo: &std::path::Path) -> CandidateFingerprint {
    CandidateFingerprint {
        head: git(repo, &["rev-parse", "HEAD"]),
        branch: git(repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
        status: git_stdout(repo, &["status", "--porcelain", "--untracked-files=all"]),
        staged_diff: git_stdout(repo, &["diff", "--cached", "--binary"]),
        working_diff: git_stdout(repo, &["diff", "HEAD", "--binary"]),
        index_file: git_stdout(repo, &["show", ":candidate.txt"]),
        worktree_file: std::fs::read(repo.join("candidate.txt")).expect("worktree file"),
        commits: git(repo, &["rev-list", "--all", "--count"]),
    }
}

fn init_candidate_repo(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).expect("repo dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("candidate.txt"), "base\n").expect("base file");
    git(&repo, &["add", "candidate.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    repo
}

fn bind_candidate_run(runtime: &OrbitRuntime, repo: &std::path::Path) -> JobRun {
    let run = insert_pending_run(runtime, "task_pr_pipeline");
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.pipeline["worktree"] = json!({"workspace_path": repo});
    runtime
        .write_run_state(&run.run_id, &state)
        .expect("write run state");
    run
}

fn assert_fingerprint_unchanged(
    repo: &std::path::Path,
    before: &CandidateFingerprint,
    label: &str,
) {
    let after = candidate_fingerprint(repo);
    assert_eq!(after.head, before.head, "{label} moved HEAD");
    assert_eq!(after.branch, before.branch, "{label} moved the branch");
    assert_eq!(after.status, before.status, "{label} changed status");
    assert_eq!(
        after.staged_diff, before.staged_diff,
        "{label} changed the index diff"
    );
    assert_eq!(
        after.working_diff, before.working_diff,
        "{label} changed the working diff"
    );
    assert_eq!(
        after.index_file, before.index_file,
        "{label} changed the index file"
    );
    assert_eq!(
        after.worktree_file, before.worktree_file,
        "{label} changed the worktree file"
    );
}

/// Unique staged content must not be reported as a complete rescue. A clean
/// tree, an unstaged edit, and a staged edit that matches the worktree still
/// keep the single documented commit.
#[test]
fn stopped_worker_rescue_records_failure_for_distinct_staged_content() {
    if run_isolated_test(std::any::type_name_of_val(
        &stopped_worker_rescue_records_failure_for_distinct_staged_content,
    )) {
        return;
    }
    let (root, runtime) = test_runtime();

    for (label, working) in [("staged-only", "base\n"), ("distinct-working", "working\n")] {
        let repo = init_candidate_repo(root.path(), label);
        std::fs::write(repo.join("candidate.txt"), "staged unique\n").expect("stage");
        git(&repo, &["add", "candidate.txt"]);
        std::fs::write(repo.join("candidate.txt"), working).expect("working copy");
        let before = candidate_fingerprint(&repo);
        assert!(
            String::from_utf8_lossy(&before.status).contains("MM candidate.txt"),
            "{label}: {}",
            String::from_utf8_lossy(&before.status)
        );
        let run = bind_candidate_run(&runtime, &repo);
        let rescue_ref = format!("refs/orbit/rescue/{}", run.run_id);
        let clause = runtime
            .keep_rescue_candidate(&run)
            .expect("preservation clause");
        assert!(
            clause.contains("could not be kept") && clause.contains("staged paths differ"),
            "{label}: {clause}"
        );
        let record = &runtime
            .read_run_state(&run.run_id)
            .expect("read state")
            .expect("state")
            .pipeline["rescue_candidate"];
        assert!(record["rescue_ref"].is_null(), "{label}: {record}");
        assert!(record["rescue_commit"].is_null(), "{label}: {record}");
        assert!(record["partial_commit"].is_null(), "{label}: {record}");
        assert!(
            record["failure"]
                .as_str()
                .is_some_and(|failure| failure.contains("staged paths differ")),
            "{label}: {record}"
        );
        assert!(
            !git_succeeds(&repo, &["rev-parse", "--verify", &rescue_ref]),
            "{label} wrote {rescue_ref}"
        );
        assert_fingerprint_unchanged(&repo, &before, label);
        assert_eq!(
            candidate_fingerprint(&repo).commits,
            before.commits,
            "{label} created a commit"
        );
    }

    let clean = init_candidate_repo(root.path(), "clean");
    let clean_before = candidate_fingerprint(&clean);
    let clean_run = bind_candidate_run(&runtime, &clean);
    let clean_ref = format!("refs/orbit/rescue/{}", clean_run.run_id);
    let clean_clause = runtime
        .keep_rescue_candidate(&clean_run)
        .expect("clean clause");
    assert!(
        clean_clause.contains(&format!("rescue candidate kept at `{clean_ref}`")),
        "{clean_clause}"
    );
    let clean_record = &runtime
        .read_run_state(&clean_run.run_id)
        .expect("read clean state")
        .expect("clean state")
        .pipeline["rescue_candidate"];
    assert_eq!(clean_record["rescue_ref"], clean_ref.as_str());
    assert_eq!(clean_record["rescue_commit"], clean_before.head.as_str());
    assert!(clean_record["partial_commit"].is_null(), "{clean_record}");
    assert!(clean_record.get("failure").is_none(), "{clean_record}");
    assert_eq!(git(&clean, &["rev-parse", &clean_ref]), clean_before.head);
    assert_fingerprint_unchanged(&clean, &clean_before, "clean");
    assert_eq!(candidate_fingerprint(&clean).commits, clean_before.commits);

    let unstaged = init_candidate_repo(root.path(), "unstaged");
    std::fs::write(unstaged.join("candidate.txt"), "working\n").expect("unstaged edit");
    let unstaged_before = candidate_fingerprint(&unstaged);
    assert!(
        String::from_utf8_lossy(&unstaged_before.status).starts_with(" M candidate.txt"),
        "{}",
        String::from_utf8_lossy(&unstaged_before.status)
    );
    let unstaged_run = bind_candidate_run(&runtime, &unstaged);
    let unstaged_ref = format!("refs/orbit/rescue/{}", unstaged_run.run_id);
    let unstaged_clause = runtime
        .keep_rescue_candidate(&unstaged_run)
        .expect("unstaged clause");
    assert!(
        unstaged_clause.contains(&format!("rescue candidate kept at `{unstaged_ref}`")),
        "{unstaged_clause}"
    );
    let unstaged_record = &runtime
        .read_run_state(&unstaged_run.run_id)
        .expect("read unstaged state")
        .expect("unstaged state")
        .pipeline["rescue_candidate"];
    let unstaged_commit = unstaged_record["rescue_commit"]
        .as_str()
        .expect("unstaged commit");
    assert_eq!(unstaged_record["partial_commit"], unstaged_commit);
    assert_eq!(
        git(&unstaged, &["rev-parse", &format!("{unstaged_commit}^")]),
        unstaged_before.head
    );
    assert_eq!(
        git_stdout(
            &unstaged,
            &["show", &format!("{unstaged_commit}:candidate.txt")]
        ),
        b"working\n"
    );
    assert_eq!(
        git(&unstaged, &["rev-parse", &unstaged_ref]),
        unstaged_commit
    );
    assert_fingerprint_unchanged(&unstaged, &unstaged_before, "unstaged");

    let matching = init_candidate_repo(root.path(), "matching-staged");
    std::fs::write(matching.join("candidate.txt"), "staged unique\n").expect("matching edit");
    git(&matching, &["add", "candidate.txt"]);
    let matching_before = candidate_fingerprint(&matching);
    assert!(
        String::from_utf8_lossy(&matching_before.status).starts_with("M  candidate.txt"),
        "{}",
        String::from_utf8_lossy(&matching_before.status)
    );
    let matching_run = bind_candidate_run(&runtime, &matching);
    let matching_ref = format!("refs/orbit/rescue/{}", matching_run.run_id);
    let matching_clause = runtime
        .keep_rescue_candidate(&matching_run)
        .expect("matching clause");
    assert!(
        matching_clause.contains(&format!("rescue candidate kept at `{matching_ref}`")),
        "{matching_clause}"
    );
    let matching_record = &runtime
        .read_run_state(&matching_run.run_id)
        .expect("read matching state")
        .expect("matching state")
        .pipeline["rescue_candidate"];
    let matching_commit = matching_record["rescue_commit"]
        .as_str()
        .expect("matching commit");
    assert_eq!(matching_record["partial_commit"], matching_commit);
    assert_eq!(
        git(&matching, &["rev-parse", &format!("{matching_commit}^")]),
        matching_before.head
    );
    assert_eq!(
        git_stdout(
            &matching,
            &["show", &format!("{matching_commit}:candidate.txt")]
        ),
        b"staged unique\n"
    );
    assert_eq!(
        git(&matching, &["rev-parse", &matching_ref]),
        matching_commit
    );
    assert_fingerprint_unchanged(&matching, &matching_before, "matching-staged");
}
