#![allow(missing_docs)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Task-worktree lifecycle through the engine's deterministic actions.
//!
//! Each test drives the shipped actions over a real fixture repository:
//! `worktree_setup` creates the run's checkout, `candidate_resume` applies a
//! requeued task's preserved candidate onto it, `pr_prepare` / `git_rebase`
//! carry its candidate onto an advanced base, the `pr_conflict_recovery`
//! leaf finishes a stopped rebase through a substitute provider CLI, and
//! `worktree_gc` decides which checkouts a finished run may give back.
//!
//! The worktree path and every Git call read process-global state (the
//! environment, `$HOME` Git config), so each test body re-runs in an isolated
//! copy of this binary with its own `$HOME` and `$TMPDIR`, no inherited
//! `ORBIT_*` / `GIT_*` variables, and a bounded wait that kills and reaps the
//! child on timeout or panic.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use orbit_agent::loop_engine::InMemorySink;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::{
    DispatchError, ResolvedCliExecutor, RuntimeHost, TaskAutomationUpdate, V2AuditWriter,
    V2DispatchInput, WorktreeGcTaskLookup, dispatch_v2_activity, execute_deterministic_action,
};
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, ContextWideningStep, ExternalRef, Task, TaskHistoryEntry,
    TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::activity_job::{ActivityV2Spec, AgentLoopSpec, OnDenial, Provider};
use orbit_types::workflow::{FailureActivityCheckpoint, JobRun, JobRunState, PipelineState};
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

/// [ORB-13920] A replica's GC: a settled claim licenses removal without any
/// task answer; a missing owner route and an owner transport failure are told
/// apart and each carries its reason; a directory Git does not list as a
/// worktree is kept with its remedy.
#[test]
fn replica_worktree_gc_reclaims_settled_claims_and_says_why_it_keeps_the_rest() {
    isolated(
        "replica_worktree_gc_reclaims_settled_claims_and_says_why_it_keeps_the_rest",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let setup = |run_id: &str, task_id: &str| {
                host.add_task(task_id, TaskStatus::Backlog);
                let input = setup_input(&[task_id], run_id);
                let setup = action(&host, "worktree_setup", &input).expect("worktree setup");
                host.add_run(job_run(run_id, JobRunState::Success, input));
                Checkout::from_setup(&setup)
            };

            // The owner would not answer about this task; its settled claim
            // is enough.
            let settled = setup("jrun-claim-settled", "T-CLAIM-SETTLED");
            host.settle_claim(
                "jrun-claim-settled",
                "claim settled with its owner hm_owner/ws",
            );
            host.answer(
                "T-CLAIM-SETTLED",
                WorktreeGcTaskLookup::OwnerUnreachable("asked anyway".into()),
            );
            let no_route = setup("jrun-claim-no-route", "T-CLAIM-NO-ROUTE");
            host.answer(
                "T-CLAIM-NO-ROUTE",
                WorktreeGcTaskLookup::NoOwnerRoute("no federated owner route".into()),
            );
            let unreachable = setup("jrun-claim-unreachable", "T-CLAIM-UNREACHABLE");
            host.answer(
                "T-CLAIM-UNREACHABLE",
                WorktreeGcTaskLookup::OwnerUnreachable("ssh: Connection timed out".into()),
            );
            let owner_error = setup("jrun-claim-owner-error", "T-CLAIM-OWNER-ERROR");
            host.answer(
                "T-CLAIM-OWNER-ERROR",
                WorktreeGcTaskLookup::OwnerLookupFailed(
                    "hm_owner/ws: remote tool failed (execution_failed): store busy".into(),
                ),
            );
            // A settled task's checkout that Git no longer lists.
            let moved = setup("jrun-claim-moved", "T-CLAIM-MOVED");
            host.set_status("T-CLAIM-MOVED", TaskStatus::Done);
            git(
                &fixture.repo,
                &["worktree", "remove", "--force", path_str(&moved.path)],
            );
            fs::create_dir_all(&moved.path).unwrap();
            fs::write(moved.path.join("notes.txt"), "left behind").unwrap();

            let cases = [
                (
                    "jrun-claim-settled",
                    "removed",
                    "claim settled with its owner",
                ),
                (
                    "jrun-claim-no-route",
                    "skipped:no_owner_route",
                    "no federated owner route",
                ),
                (
                    "jrun-claim-unreachable",
                    "skipped:owner_unreachable",
                    "Connection timed out",
                ),
                (
                    "jrun-claim-owner-error",
                    "skipped:owner_lookup_failed",
                    "store busy",
                ),
                (
                    "jrun-claim-moved",
                    "skipped:not_registered_worktree",
                    "git worktree repair",
                ),
            ];
            for (run_id, expected_action, expected_detail) in cases {
                let result = action(&host, "worktree_gc", &json!({"target_run_id": run_id}))
                    .expect("worktree gc");
                let report = &result["reports"][0];
                assert_eq!(report["run_id"], run_id, "{result:#}");
                assert_eq!(report["action"], expected_action, "{run_id}: {report:#}");
                assert!(
                    report["detail"]
                        .as_str()
                        .is_some_and(|detail| detail.contains(expected_detail)),
                    "{run_id}: the report says why: {report:#}"
                );
            }
            assert!(
                !settled.path.exists(),
                "the settled claim's checkout is removed"
            );
            assert!(
                git(&fixture.repo, &["branch", "--list", &settled.branch]).is_empty(),
                "and its branch"
            );
            for kept in [&no_route, &unreachable] {
                assert!(registered_worktrees(&fixture.repo).contains(&kept.path));
            }
            assert!(owner_error.path.exists(), "an owner response error is kept");
            assert!(moved.path.join("notes.txt").exists(), "never removed by GC");
        },
    );
}

/// One failed owner route must not suppress lookups to another owner in the
/// same worktree GC sweep.
#[test]
fn replica_worktree_gc_fences_unreachable_owners_by_route() {
    isolated(
        "replica_worktree_gc_fences_unreachable_owners_by_route",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            let setup = |run_id: &str, task_id: &str| {
                host.add_task(task_id, TaskStatus::Backlog);
                let input = setup_input(&[task_id], run_id);
                let setup = action(&host, "worktree_setup", &input).expect("worktree setup");
                host.add_run(job_run(run_id, JobRunState::Success, input));
                Checkout::from_setup(&setup)
            };
            let left = setup("jrun-owner-a", "T-OWNER-A");
            let right = setup("jrun-owner-b", "T-OWNER-B");
            let (
                unreachable,
                unreachable_run,
                unreachable_task,
                reachable,
                reachable_run,
                reachable_task,
            ) = if left.path < right.path {
                (
                    &left,
                    "jrun-owner-a",
                    "T-OWNER-A",
                    &right,
                    "jrun-owner-b",
                    "T-OWNER-B",
                )
            } else {
                (
                    &right,
                    "jrun-owner-b",
                    "T-OWNER-B",
                    &left,
                    "jrun-owner-a",
                    "T-OWNER-A",
                )
            };
            host.set_lookup_scope(unreachable_run, "hm_down/ws-a");
            host.set_lookup_scope(reachable_run, "hm_up/ws-b");
            host.answer(
                unreachable_task,
                WorktreeGcTaskLookup::OwnerUnreachable("ssh: timed out".into()),
            );
            host.answer(
                reachable_task,
                WorktreeGcTaskLookup::Found {
                    status: TaskStatus::Done,
                    pr_status: None,
                },
            );

            let result = action(&host, "worktree_gc", &json!({})).expect("worktree gc");
            let reports = result["reports"].as_array().expect("reports");
            let unreachable_report = reports
                .iter()
                .find(|report| report["run_id"] == unreachable_run)
                .expect("unreachable report");
            let reachable_report = reports
                .iter()
                .find(|report| report["run_id"] == reachable_run)
                .expect("reachable report");
            assert_eq!(
                unreachable_report["action"], "skipped:owner_unreachable",
                "{unreachable_report:#}"
            );
            assert_eq!(
                reachable_report["action"], "removed",
                "the reachable owner's done task is still checked: {reachable_report:#}"
            );
            assert!(!reachable.path.exists());
            assert!(unreachable.path.exists());
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
            // A run keeps one branch identity across retries, even when the
            // retry happens later and only the orphaned branch remains.
            std::thread::sleep(Duration::from_secs(1));
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
// candidate_resume: a requeued task resumes its preserved candidate
// ---------------------------------------------------------------------------

/// A clean candidate that passes owner validation on the advanced base is
/// delivered as is: no implementation step, and the run's own commit step
/// commits exactly the candidate's change on the new base.
#[test]
fn a_validated_candidate_is_resumed_on_the_new_base_without_implementation() {
    isolated(
        "a_validated_candidate_is_resumed_on_the_new_base_without_implementation",
        || {
            let preserved = PreservedCandidate::new("feature.txt", "feature\n");
            let base = commit_file(&preserved.fixture.repo, "base.txt", "v2\n");
            preserved.host.set_required_commands(&[
                "test -f feature.txt && read line < base.txt && test \"$line\" = v2",
            ]);
            let setup = preserved.next_setup();
            assert_eq!(setup["base_sha"], base.as_str());

            let resumed = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(resumed["outcome"], "resumed_validated", "{resumed}");
            assert_eq!(resumed["implement"], false);
            assert_eq!(resumed["source_run_id"], FAILED_RUN);
            assert_eq!(resumed["source_sha"], preserved.candidate.as_str());
            assert_eq!(resumed["repair"], Value::Null);
            let checkout = Checkout::from_setup(&setup);
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), base);
            assert_eq!(
                git(&checkout.path, &["status", "--porcelain"]),
                "?? feature.txt",
                "the candidate is applied as uncommitted work"
            );
            assert_resume_recorded(&preserved, "resumed_validated");

            let committed = action(
                &preserved.host,
                "git_commit",
                &json!({
                    "job_run_id": NEXT_RUN,
                    "scope": "all",
                    "workspace_path": checkout.path,
                    "base_ref": setup["base_ref"],
                    "base_sha": setup["base_sha"],
                }),
            )
            .expect("the run's commit step delivers the candidate");
            let head = committed["commit_sha"].as_str().unwrap();
            assert_eq!(
                git(&checkout.path, &["rev-parse", &format!("{head}^")]),
                base
            );
            assert_eq!(
                git(&checkout.path, &["show", &format!("{head}:feature.txt")]),
                "feature"
            );
        },
    );
}

/// A candidate that conflicts with the advanced base is handed to the
/// implementer as uncommitted work with the conflict markers and paths;
/// validation does not run on a conflicted tree.
#[test]
fn a_conflicting_candidate_is_handed_to_the_implementer_with_its_conflict() {
    isolated(
        "a_conflicting_candidate_is_handed_to_the_implementer_with_its_conflict",
        || {
            let preserved = PreservedCandidate::new("base.txt", "candidate\n");
            let base = commit_file(&preserved.fixture.repo, "base.txt", "v2\n");
            preserved.host.set_required_commands(&["exit 99"]);
            let setup = preserved.next_setup();

            let resumed = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
            assert_eq!(resumed["implement"], true);
            assert_eq!(resumed["repair"]["trigger"], "conflict");
            assert_eq!(resumed["repair"]["conflicting_paths"], json!(["base.txt"]));
            assert!(
                resumed["repair"]["output"]
                    .as_str()
                    .unwrap()
                    .contains("base.txt"),
                "{resumed}"
            );
            let checkout = Checkout::from_setup(&setup);
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), base);
            let conflicted = fs::read_to_string(checkout.path.join("base.txt")).unwrap();
            assert!(
                conflicted.contains("<<<<<<<")
                    && conflicted.contains("candidate")
                    && conflicted.contains("v2"),
                "the implementer starts from both sides: {conflicted}"
            );
            assert!(
                git(&checkout.path, &["diff", "--name-only", "--diff-filter=U"]).is_empty(),
                "no merge state is left behind"
            );
            assert_resume_recorded(&preserved, "resumed_repaired");
        },
    );
}

/// A clean candidate that fails owner validation is handed to the
/// implementer with the failing command and its output.
#[test]
fn a_candidate_failing_validation_is_handed_to_the_implementer_with_the_output() {
    isolated(
        "a_candidate_failing_validation_is_handed_to_the_implementer_with_the_output",
        || {
            let preserved = PreservedCandidate::new("feature.txt", "feature\n");
            commit_file(&preserved.fixture.repo, "base.txt", "v2\n");
            let command = "echo 'feature.txt is wrong' >&2; exit 3";
            preserved.host.set_required_commands(&[command]);
            let setup = preserved.next_setup();

            let resumed = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
            assert_eq!(resumed["implement"], true);
            assert_eq!(resumed["repair"]["trigger"], "validation");
            assert_eq!(resumed["repair"]["command"], command);
            assert_eq!(resumed["repair"]["exit_code"], 3);
            assert!(
                resumed["repair"]["output"]
                    .as_str()
                    .unwrap()
                    .contains("feature.txt is wrong"),
                "{resumed}"
            );
            let checkout = Checkout::from_setup(&setup);
            assert_eq!(
                fs::read_to_string(checkout.path.join("feature.txt")).unwrap(),
                "feature\n",
                "the implementer starts from the candidate"
            );
            assert_resume_recorded(&preserved, "resumed_repaired");
        },
    );
}

/// A spec change since the candidate's run, or an operator discard recorded
/// since that run began, forces a fresh implementation with the reason, and
/// leaves the checkout untouched. A discard older than the run does not.
#[test]
fn a_changed_spec_or_an_operator_discard_implements_fresh() {
    isolated(
        "a_changed_spec_or_an_operator_discard_implements_fresh",
        || {
            let preserved = PreservedCandidate::new("feature.txt", "feature\n");
            let base = commit_file(&preserved.fixture.repo, "base.txt", "v2\n");
            let setup = preserved.next_setup();
            let checkout = Checkout::from_setup(&setup);
            let untouched = || {
                assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), base);
                assert!(git(&checkout.path, &["status", "--porcelain"]).is_empty());
            };

            preserved
                .host
                .set_description(RESUME_TASK, "A re-scoped task.");
            let changed = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(changed["outcome"], "fresh", "{changed}");
            assert_eq!(changed["implement"], true);
            assert!(
                changed["reason"]
                    .as_str()
                    .unwrap()
                    .contains("changed since"),
                "{changed}"
            );
            assert_eq!(changed["source_sha"], preserved.candidate.as_str());
            untouched();
            assert_resume_recorded(&preserved, "fresh");

            preserved.host.set_description(RESUME_TASK, "");
            preserved.host.clear_history(RESUME_TASK);
            preserved
                .host
                .record_history(RESUME_TASK, discard_entry(Utc::now()));
            let discarded = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(discarded["outcome"], "fresh", "{discarded}");
            assert!(
                discarded["reason"].as_str().unwrap().contains("discarded"),
                "{discarded}"
            );
            untouched();
            assert_resume_recorded(&preserved, "fresh");

            preserved.host.clear_history(RESUME_TASK);
            preserved.host.record_history(
                RESUME_TASK,
                discard_entry(Utc::now() - chrono::Duration::hours(1)),
            );
            let resumed = preserved.resume(&setup).expect("candidate_resume");
            assert_eq!(
                resumed["outcome"], "resumed_validated",
                "a discard from before the candidate's run does not apply: {resumed}"
            );
        },
    );
}

const RESUME_TASK: &str = "T-RESUME";
const FAILED_RUN: &str = "jrun-failed";
const NEXT_RUN: &str = "jrun-next";

/// A task whose run `FAILED_RUN` committed a candidate and failed after
/// implementation, with the failure handoff's preservation record.
struct PreservedCandidate {
    fixture: Fixture,
    host: LifecycleHost,
    candidate: String,
}

impl PreservedCandidate {
    fn new(file: &str, contents: &str) -> Self {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        host.add_task(RESUME_TASK, TaskStatus::Backlog);
        let setup = action(
            &host,
            "worktree_setup",
            &setup_input(&[RESUME_TASK], FAILED_RUN),
        )
        .expect("the failed run's setup");
        let checkout = Checkout::from_setup(&setup);
        let candidate = commit_file(&checkout.path, file, contents);
        host.add_run(job_run(
            FAILED_RUN,
            JobRunState::Failed,
            json!({ "task_ids": [RESUME_TASK] }),
        ));
        host.preserve(
            FAILED_RUN,
            "validate",
            json!({
                "phase": "failure_handoff",
                "decision": "blocked_failure_pr",
                "task_id": RESUME_TASK,
                "handoff_run_id": FAILED_RUN,
                "branch": checkout.branch,
                "head_sha": candidate,
                "task_spec_digest": host.get_task(RESUME_TASK).unwrap().spec_digest(),
            }),
        );
        host.set_status(RESUME_TASK, TaskStatus::Backlog);
        Self {
            fixture,
            host,
            candidate,
        }
    }

    /// The requeued task's next run sets up its checkout, linked to the
    /// failed run.
    fn next_setup(&self) -> Value {
        let setup = action(
            &self.host,
            "worktree_setup",
            &setup_input(&[RESUME_TASK], NEXT_RUN),
        )
        .expect("the next run's setup");
        assert_eq!(setup["prior_job_run_id"], FAILED_RUN);
        setup
    }

    fn resume(&self, setup: &Value) -> Result<Value, OrbitError> {
        action(
            &self.host,
            "candidate_resume",
            &json!({
                "job_run_id": NEXT_RUN,
                "task_ids": [RESUME_TASK],
                "workspace_path": setup["workspace_path"],
                "base_sha": setup["base_sha"],
                "prior_job_run_id": setup["prior_job_run_id"],
            }),
        )
    }
}

/// The task history names the outcome, the source run and the candidate SHA.
fn assert_resume_recorded(preserved: &PreservedCandidate, outcome: &str) {
    let history = preserved.host.history(RESUME_TASK);
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.event == "candidate_resume")
        .unwrap_or_else(|| panic!("no candidate_resume event in {history:?}"));
    let note = entry.note.as_deref().unwrap_or_default();
    for expected in [
        format!("{outcome}:"),
        format!("source_run={FAILED_RUN}"),
        format!("source_sha={}", preserved.candidate),
    ] {
        assert!(note.contains(&expected), "{expected} in {note}");
    }
}

fn discard_entry(at: chrono::DateTime<Utc>) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at,
        by: "human:operator".to_string(),
        event: CANDIDATE_DISCARDED_EVENT.to_string(),
        note: None,
        from_status: None,
        to_status: None,
    }
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

/// A provider shell that runs `body` in the checkout and reports success.
fn provider_script(body: &str) -> String {
    format!(
        "#!/bin/sh\nset -eu\ncat > /dev/null\n{body}\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n"
    )
}

/// The recovery input for `prepared`'s stopped rebase.
fn conflict_recovery_input(prepared: &PreparedRebase, conflict: &Value) -> Value {
    json!({
        "prompt": "resolve the stopped rebase",
        "task_id": "T-REBASE",
        "workspace_path": prepared.checkout.path,
        "repo_root": prepared.checkout.path,
        "run_id": prepared.run_id,
        "failed_step_id": "sync_base",
        "activity_name": "git_rebase",
        "recovery_kind": "vcs_conflict",
        "operation": conflict["operation"],
        "original_base_sha": conflict["original_base_sha"],
        "target_base_sha": conflict["target_base_sha"],
        "conflicting_paths": conflict["conflicting_paths"],
        "failed_step_input": {
            "head": prepared.prepared["head"],
            "head_sha": prepared.candidate,
            "base_ref": prepared.prepared["base_ref"],
            "base_sha": conflict["target_base_sha"],
        },
    })
}

/// Stop `prepared`'s rebase on its overlapping edit, as the recovery input's
/// conflict fields.
fn stopped_conflict(prepared: &PreparedRebase) -> Value {
    let OrbitError::RecoverableVcsConflict(conflict) =
        prepared.rebase().expect_err("overlapping rebase")
    else {
        panic!("expected a recoverable conflict");
    };
    json!({
        "operation": conflict.operation,
        "original_base_sha": conflict.original_base_sha,
        "target_base_sha": conflict.target_base_sha,
        "conflicting_paths": conflict.conflicting_paths,
    })
}

/// [ORB-13990] The ORB-13906 shape: resolving an upstream export conflict
/// needs a companion module move outside the conflict set. The host stages
/// the resolution with every companion change (the moved file and the
/// removed original), continues the rebase, and widens the task's selectors
/// with recovery provenance. Scratch under `.orbit/tmp/` is never staged.
#[cfg(unix)]
#[test]
fn conflict_recovery_commits_companion_edits_with_the_resolution() {
    isolated(
        "conflict_recovery_commits_companion_edits_with_the_resolution",
        || {
            let prepared = PreparedRebase::new("jrun-companion", "README.md", "README.md");
            let target = prepared.target.clone();
            let conflict = stopped_conflict(&prepared);
            let provider = prepared.fixture.root.path().join("codex");
            write_executable(
                &provider,
                &provider_script(
                    "printf 'pub use moved::export;\\n' > README.md\nmkdir -p src/moved .orbit/tmp\nmv base.txt src/moved/mod.rs\nprintf 'scratch\\n' > .orbit/tmp/notes.md",
                ),
            );
            let host = prepared.host.with_provider(&provider);

            let outcome = recover(
                &host,
                &prepared.run_id,
                conflict_recovery_input(&prepared, &conflict),
            )
            .expect("a resolution with companion edits continues the rebase");
            assert!(outcome.success, "{:?}", outcome.message);
            assert!(!rebase_in_progress(&prepared.checkout.path));
            assert!(is_ancestor(&prepared.checkout.path, &target, "HEAD"));
            assert_eq!(
                git(
                    &prepared.checkout.path,
                    &["show", "--format=", "--name-only", "--no-renames", "HEAD"]
                ),
                "README.md\nbase.txt\nsrc/moved/mod.rs",
                "the continued commit holds the resolution and both sides of the move"
            );
            assert_eq!(git(&prepared.checkout.path, &["status", "--porcelain"]), "");
            assert_eq!(
                fs::read_to_string(prepared.checkout.path.join(".orbit/tmp/notes.md")).unwrap(),
                "scratch\n",
                "scratch stays in place and out of the commit"
            );
            let checkpoints = host.checkpoints();
            let [(_, _, checkpoint)] = checkpoints.as_slice() else {
                panic!("expected one recovery checkpoint, got {checkpoints:#?}");
            };
            assert_eq!(
                checkpoint["companion_paths"],
                json!(["base.txt", "src/moved/mod.rs"])
            );
            assert_eq!(
                host.widenings(),
                vec![(
                    "T-REBASE".to_string(),
                    ContextWideningStep::Recovery,
                    "pr_conflict_recovery".to_string(),
                    vec!["base.txt".to_string(), "src/moved/mod.rs".to_string()],
                )]
            );
        },
    );
}

/// [ORB-13990] Companion edits never excuse the conflict itself: a conflict
/// path left unrepaired, or repaired with markers still in it, refuses the
/// continuation with the rebase left stopped and nothing widened. Staging
/// stays host-owned: a provider that stages its own companion is refused.
#[cfg(unix)]
#[test]
fn conflict_recovery_still_refuses_unresolved_conflicts_beside_companion_edits() {
    isolated(
        "conflict_recovery_still_refuses_unresolved_conflicts_beside_companion_edits",
        || {
            for (run_id, resolution, refusal) in [
                (
                    "jrun-unrepaired",
                    "",
                    "authorized conflict files were not repaired",
                ),
                (
                    "jrun-markers",
                    "printf '<<<<<<< ours\\na\\n=======\\nb\\n>>>>>>> theirs\\n' > README.md\n",
                    "still contains conflict markers",
                ),
                (
                    "jrun-staged",
                    "printf 'resolved\\n' > README.md\nprintf 'companion\\n' > companion.txt\ngit add companion.txt\n",
                    "changed HEAD, branch, or index",
                ),
            ] {
                let prepared = PreparedRebase::new(run_id, "README.md", "README.md");
                let conflict = stopped_conflict(&prepared);
                let provider = prepared.fixture.root.path().join("codex");
                write_executable(
                    &provider,
                    &provider_script(&format!(
                        "{resolution}printf 'companion\\n' > companion.txt"
                    )),
                );
                let host = prepared.host.with_provider(&provider);

                let error = recover(
                    &host,
                    &prepared.run_id,
                    conflict_recovery_input(&prepared, &conflict),
                )
                .expect_err("an unresolved conflict path refuses continuation");
                assert!(error.to_string().contains(refusal), "{run_id}: {error}");
                assert!(rebase_in_progress(&prepared.checkout.path), "{run_id}");
                assert!(host.checkpoints().is_empty(), "{run_id}");
                assert!(host.widenings().is_empty(), "{run_id}");
                assert_eq!(
                    fs::read_to_string(prepared.checkout.path.join("companion.txt")).unwrap(),
                    "companion\n",
                    "{run_id}: the provider's bytes stay for diagnosis"
                );
            }
        },
    );
}

/// [ORB-13990] Implementer and recovery agents may change any path the work
/// requires. As each exits, the boundary records the paths it changed on its
/// task, with the step that introduced them; host-owned `.orbit/` state and
/// gitignored output are never attributed. The reviewer's changes widen at
/// review settlement instead.
#[cfg(unix)]
#[test]
fn agent_changed_paths_widen_selectors_with_their_step_as_the_agent_exits() {
    isolated(
        "agent_changed_paths_widen_selectors_with_their_step_as_the_agent_exits",
        || {
            for (index, (activity, step)) in [
                ("agent_implement", Some(ContextWideningStep::Implement)),
                ("step_failure_recovery", Some(ContextWideningStep::Recovery)),
                ("final_recovery", Some(ContextWideningStep::Recovery)),
                ("agent_review_repair", None),
            ]
            .into_iter()
            .enumerate()
            {
                let run_id = format!("jrun-widen-{index}");
                let fixture = Fixture::new();
                fs::write(fixture.repo.join(".gitignore"), ".orbit/\ntarget/\n").unwrap();
                git(&fixture.repo, &["commit", "-am", "ignore build output"]);
                let host = LifecycleHost::new(&fixture.repo);
                host.add_task("T-WIDEN", TaskStatus::Backlog);
                let setup = action(&host, "worktree_setup", &setup_input(&["T-WIDEN"], &run_id))
                    .expect("worktree setup");
                let checkout = Checkout::from_setup(&setup);
                let provider = fixture.root.path().join("codex");
                write_executable(
                    &provider,
                    &provider_script(
                        "printf 'edited\\n' > README.md\nmkdir -p tests .orbit/tmp target\nprintf 'new\\n' > tests/new_case.rs\nprintf 'scratch\\n' > .orbit/tmp/log\nprintf 'built\\n' > target/out",
                    ),
                );
                let host = host.with_provider(&provider);

                let outcome =
                    dispatch_linked_activity(&host, activity, &run_id, "T-WIDEN", &checkout.path)
                        .unwrap_or_else(|error| panic!("{activity} may change any path: {error}"));
                assert!(outcome.success, "{activity}: {:?}", outcome.message);
                let expected = step
                    .map(|step| {
                        vec![(
                            "T-WIDEN".to_string(),
                            step,
                            activity.to_string(),
                            vec!["README.md".to_string(), "tests/new_case.rs".to_string()],
                        )]
                    })
                    .unwrap_or_default();
                assert_eq!(host.widenings(), expected, "{activity}");
            }
        },
    );
}

/// A provider that writes the primary checkout fails the boundary, and both
/// trees keep the bytes the provider left. Disjoint source dirt is enough:
/// overlap with the candidate is not what makes the edit fatal.
#[cfg(unix)]
#[test]
fn provider_primary_source_edit_fails_closed_and_preserves_both_checkouts() {
    isolated(
        "provider_primary_source_edit_fails_closed_and_preserves_both_checkouts",
        || {
            let fixture = Fixture::new();
            let host = LifecycleHost::new(&fixture.repo);
            host.add_task("T-ESCAPE", TaskStatus::Backlog);
            let setup = action(
                &host,
                "worktree_setup",
                &setup_input(&["T-ESCAPE"], "jrun-primary-escape"),
            )
            .expect("worktree setup");
            let checkout = Checkout::from_setup(&setup);
            let primary_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
            let assigned_head = git(&checkout.path, &["rev-parse", "HEAD"]);

            let provider = fixture.root.path().join("codex");
            let primary = fixture.repo.display().to_string();
            write_executable(
                &provider,
                &format!(
                    "#!/bin/sh\nset -eu\ncat > /dev/null\nprintf 'assigned stays\\n' > assigned-stays.txt\nprintf 'primary readme\\n' > '{primary}/README.md'\nprintf 'escaped\\n' > '{primary}/escaped.txt'\ngit -C '{primary}' add -- README.md escaped.txt\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
                    primary = primary,
                ),
            );
            let host = host.with_provider(&provider);
            let error =
                dispatch_linked_provider(&host, "jrun-primary-escape", "T-ESCAPE", &checkout.path)
                    .expect_err("a primary source edit is a worktree-boundary failure");

            let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
            assert!(
                error.is_non_retryable(),
                "primary drift must not be retried as a transient spawn failure"
            );
            assert_eq!(
                diagnostic["conflicting_paths"],
                json!([]),
                "disjoint source dirt is fatal on its path class"
            );
            assert_eq!(
                diagnostic["primary_dirt_paths"],
                json!(["README.md", "escaped.txt"]),
                "the diagnostic names the stationary primary source edits"
            );
            assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), primary_head);
            assert_eq!(git(&checkout.path, &["rev-parse", "HEAD"]), assigned_head);
            assert_eq!(
                fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
                "primary readme\n"
            );
            assert_eq!(
                fs::read_to_string(fixture.repo.join("escaped.txt")).unwrap(),
                "escaped\n"
            );
            assert_eq!(
                staged_paths(&fixture.repo),
                BTreeSet::from(["README.md".to_string(), "escaped.txt".to_string()]),
                "the guard must not reset the provider-mutated primary index"
            );
            assert_eq!(
                fs::read_to_string(checkout.path.join("assigned-stays.txt")).unwrap(),
                "assigned stays\n"
            );
            assert!(
                !checkout.path.join("escaped.txt").exists(),
                "the guard must not copy the primary edit into the assigned checkout"
            );
            assert_eq!(
                fs::read_to_string(checkout.path.join("README.md")).unwrap(),
                "base\n",
                "the assigned candidate keeps the bytes it had"
            );
        },
    );
}

/// Record-store dirt on a path the run also changed is still drift. The
/// `.orbit/` prefix alone must not excuse an intersection with the candidate.
#[cfg(unix)]
#[test]
fn stationary_record_store_dirt_overlapping_the_run_fails_closed() {
    isolated(
        "stationary_record_store_dirt_overlapping_the_run_fails_closed",
        || {
            let fixture = Fixture::new();
            let record = ".orbit/auto_tasks/nightly.yaml";
            commit_tracked_record(&fixture.repo, record, "name: nightly\n");
            let host = LifecycleHost::new(&fixture.repo);
            host.add_task("T-OVERLAP", TaskStatus::Backlog);
            let setup = action(
                &host,
                "worktree_setup",
                &setup_input(&["T-OVERLAP"], "jrun-record-overlap"),
            )
            .expect("worktree setup");
            let checkout = Checkout::from_setup(&setup);
            let primary_head = git(&fixture.repo, &["rev-parse", "HEAD"]);

            let provider = fixture.root.path().join("codex");
            let primary = fixture.repo.display().to_string();
            write_executable(
                &provider,
                &format!(
                    "#!/bin/sh\nset -eu\ncat > /dev/null\nmkdir -p .orbit/auto_tasks\nprintf 'name: from-run\\n' > '{record}'\nprintf 'name: from-primary\\n' > '{primary}/{record}'\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
                    record = record,
                    primary = primary,
                ),
            );
            let host = host.with_provider(&provider);
            let error =
                dispatch_linked_provider(&host, "jrun-record-overlap", "T-OVERLAP", &checkout.path)
                    .expect_err("record-store dirt on a run path is a boundary failure");

            let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
            assert_eq!(diagnostic["conflicting_paths"], json!([record]));
            assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), primary_head);
            assert_eq!(
                fs::read_to_string(fixture.repo.join(record)).unwrap(),
                "name: from-primary\n"
            );
            assert_eq!(
                fs::read_to_string(checkout.path.join(record)).unwrap(),
                "name: from-run\n"
            );
        },
    );
}

/// Concurrent record-store dirt that does not touch the run stays on the
/// primary and does not fail the provider. The provider itself only edits
/// the assigned checkout.
#[cfg(unix)]
#[test]
fn stationary_record_store_dirt_disjoint_from_the_run_stays_benign() {
    isolated(
        "stationary_record_store_dirt_disjoint_from_the_run_stays_benign",
        || {
            let fixture = Fixture::new();
            let record = ".orbit/auto_tasks/nightly.yaml";
            commit_tracked_record(&fixture.repo, record, "name: nightly\n");
            let host = LifecycleHost::new(&fixture.repo);
            host.add_task("T-RECORD", TaskStatus::Backlog);
            let setup = action(
                &host,
                "worktree_setup",
                &setup_input(&["T-RECORD"], "jrun-record-dirt"),
            )
            .expect("worktree setup");
            let checkout = Checkout::from_setup(&setup);
            let primary_head = git(&fixture.repo, &["rev-parse", "HEAD"]);
            let ready = fixture.root.path().join("record-ready");
            let go = fixture.root.path().join("record-go");
            let primary_record = fixture.repo.join(record);

            let provider = fixture.root.path().join("codex");
            write_executable(
                &provider,
                &format!(
                    "#!/bin/sh\nset -eu\ncat > /dev/null\n: > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.05; done\nprintf 'candidate\\n' > candidate.txt\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n",
                    ready.display(),
                    go.display(),
                ),
            );
            let host = host.with_provider(&provider);
            let curator = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                while !ready.exists() {
                    assert!(
                        Instant::now() < deadline,
                        "provider never signaled that the primary snapshot was taken"
                    );
                    thread::sleep(Duration::from_millis(20));
                }
                fs::write(&primary_record, "name: nightly\nenabled: true\n").unwrap();
                fs::write(&go, "go\n").unwrap();
            });

            let outcome =
                dispatch_linked_provider(&host, "jrun-record-dirt", "T-RECORD", &checkout.path)
                    .expect("disjoint record-store dirt is not primary drift");
            curator.join().expect("curator thread");
            assert!(outcome.success, "{:?}", outcome.message);
            assert_eq!(git(&fixture.repo, &["rev-parse", "HEAD"]), primary_head);
            assert_eq!(
                fs::read_to_string(fixture.repo.join(record)).unwrap(),
                "name: nightly\nenabled: true\n",
                "the guard must leave concurrent record-store dirt in place"
            );
            assert_eq!(
                fs::read_to_string(fixture.repo.join("README.md")).unwrap(),
                "base\n"
            );
            assert_eq!(
                fs::read_to_string(checkout.path.join("candidate.txt")).unwrap(),
                "candidate\n"
            );
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

/// Dispatch a substitute provider in a linked worktree whose registered
/// primary is the host repository.
fn dispatch_linked_provider(
    host: &LifecycleHost,
    run_id: &str,
    task_id: &str,
    workspace: &Path,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    dispatch_linked_activity(host, "agent_implement", run_id, task_id, workspace)
}

/// Dispatch a substitute provider as `activity_name` in a linked worktree.
fn dispatch_linked_activity(
    host: &LifecycleHost,
    activity_name: &str,
    run_id: &str,
    task_id: &str,
    workspace: &Path,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let blobs = TempDir::new().unwrap();
    let audit = Arc::new(V2AuditWriter::new(
        run_id,
        "codex:test-model",
        Arc::new(InMemorySink::new(blobs.path().to_path_buf())),
    ));
    let spec = ActivityV2Spec::AgentLoop(AgentLoopSpec {
        tool_disallow_list: None,
        instruction: "Edit the assigned checkout.".to_string(),
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
        activity_name,
        spec: &spec,
        fs_profile: None,
        input: json!({
            "prompt": "implement",
            "task_id": task_id,
            "workspace_path": workspace,
            "repo_root": workspace,
            "run_id": run_id,
        }),
        audit,
        run_id,
        host: Some(host),
    })
}

fn integrity_diagnostic(error: &DispatchError, code: &str) -> Value {
    let DispatchError::WorktreeIntegrity {
        code: actual,
        diagnostic,
    } = error
    else {
        panic!("expected a worktree integrity error, got {error:?}");
    };
    assert_eq!(*actual, code, "{error}");
    serde_json::from_str(diagnostic).expect("integrity diagnostic is json")
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
    /// A replica owner's answer per task, overriding the local task store.
    owner_answers: Mutex<BTreeMap<String, WorktreeGcTaskLookup>>,
    /// Claimed runs whose claim is settled, with the settlement's account.
    settled_claims: Mutex<BTreeMap<String, String>>,
    /// The owner route of each claimed run for GC memoization tests.
    lookup_scopes: Mutex<BTreeMap<String, String>>,
    /// Selector widenings requested, as (task, step, activity, paths).
    widenings: Mutex<Vec<Widening>>,
    /// Durable run state by run id.
    run_states: Mutex<BTreeMap<String, PipelineState>>,
    /// Task history events by task id.
    history: Mutex<BTreeMap<String, Vec<TaskHistoryEntry>>>,
    /// `workflow.required_validation_commands`.
    required_commands: Mutex<Vec<String>>,
}

type Widening = (String, ContextWideningStep, String, Vec<String>);

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

    fn answer(&self, task_id: &str, lookup: WorktreeGcTaskLookup) {
        self.owner_answers
            .lock()
            .unwrap()
            .insert(task_id.to_string(), lookup);
    }

    fn settle_claim(&self, run_id: &str, settlement: &str) {
        self.settled_claims
            .lock()
            .unwrap()
            .insert(run_id.to_string(), settlement.to_string());
    }

    fn set_lookup_scope(&self, run_id: &str, scope: &str) {
        self.lookup_scopes
            .lock()
            .unwrap()
            .insert(run_id.to_string(), scope.to_string());
    }

    fn admitted(&self) -> Vec<String> {
        self.admitted.lock().unwrap().clone()
    }

    fn checkpoints(&self) -> Vec<(String, String, Value)> {
        self.checkpoints.lock().unwrap().clone()
    }

    fn widenings(&self) -> Vec<Widening> {
        self.widenings.lock().unwrap().clone()
    }

    fn set_description(&self, id: &str, description: &str) {
        self.tasks.lock().unwrap().get_mut(id).unwrap().description = description.to_string();
    }

    fn set_required_commands(&self, commands: &[&str]) {
        *self.required_commands.lock().unwrap() =
            commands.iter().map(ToString::to_string).collect();
    }

    /// Record `output` as `run_id`'s failure-activity checkpoint.
    fn preserve(&self, run_id: &str, failed_step_id: &str, output: Value) {
        let mut state = PipelineState::new(
            run_id.to_string(),
            "task_pr_pipeline".to_string(),
            json!({}),
        );
        state.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
            activity_name: "pr_failure_handoff".to_string(),
            failed_step_id: failed_step_id.to_string(),
            output,
        });
        self.run_states
            .lock()
            .unwrap()
            .insert(run_id.to_string(), state);
    }

    fn record_history(&self, task_id: &str, entry: TaskHistoryEntry) {
        self.history
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .push(entry);
    }

    fn clear_history(&self, task_id: &str) {
        self.history.lock().unwrap().remove(task_id);
    }

    fn history(&self, task_id: &str) -> Vec<TaskHistoryEntry> {
        self.history
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .unwrap_or_default()
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
        drop(tasks);
        if let Some(event) = update.status_event {
            self.record_history(
                task_id,
                TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".to_string(),
                    event,
                    note: update.status_note,
                    from_status: None,
                    to_status: None,
                },
            );
        }
        Ok(())
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(self.history(task_id))
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Ok(self
            .runs
            .lock()
            .unwrap()
            .iter()
            .find(|run| run.run_id == run_id)
            .cloned())
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        Ok(self.run_states.lock().unwrap().get(run_id).cloned())
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.required_commands.lock().unwrap().clone()
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(self.runs.lock().unwrap().clone())
    }

    fn lookup_task_for_worktree_gc(&self, _run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        if let Some(answer) = self.owner_answers.lock().unwrap().get(task_id) {
            return answer.clone();
        }
        match self.get_task(task_id) {
            Ok(task) => WorktreeGcTaskLookup::Found {
                status: task.status,
                pr_status: task.pr_status,
            },
            Err(_) => WorktreeGcTaskLookup::Unresolved,
        }
    }

    fn worktree_gc_task_lookup_scope(&self, run_id: &str) -> Option<String> {
        Some(
            self.lookup_scopes
                .lock()
                .unwrap()
                .get(run_id)
                .cloned()
                .unwrap_or_else(|| "local".to_string()),
        )
    }

    fn settled_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        self.settled_claims.lock().unwrap().get(run_id).cloned()
    }

    fn widen_task_context_files(
        &self,
        task_id: &str,
        _run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        self.widenings.lock().unwrap().push((
            task_id.to_string(),
            step,
            activity.to_string(),
            paths.to_vec(),
        ));
        Ok(paths.iter().map(|path| format!("file:{path}")).collect())
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

/// Commit `file` even when `.gitignore` would hide it, creating parents first.
fn commit_tracked_record(repo: &Path, file: &str, contents: &str) {
    let target = repo.join(file);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, contents).unwrap();
    git(repo, &["add", "-f", "--", file]);
    git(repo, &["commit", "-m", &format!("track {file}")]);
}

fn staged_paths(repo: &Path) -> BTreeSet<String> {
    git(repo, &["diff", "--cached", "--name-only"])
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
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
    orbit_common::test_env::assert_child_test_passed(
        &qualified,
        status,
        fs::read(&log_path).unwrap(),
        [],
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
