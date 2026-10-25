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
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RebaseRecoveryAttemptScope, ResolvedCliExecutor, RuntimeHost,
    StepRecoveryAdmission, TaskAutomationUpdate, V2AuditWriter, V2DispatchInput,
    WorktreeGcTaskLookup, dispatch_v2_activity, execute_deterministic_action,
};
use orbit_store::contracts::{ClaimCandidateRef, KeptClaimCandidate};
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, ContextWideningStep, ExternalRef, Task, TaskArtifact,
    TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::workflow::activity_job::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, OnDenial, Provider,
};
use orbit_types::workflow::{
    ConventionTemplate, FailureActivityCheckpoint, FinalRecoveryDecision, JobRun, JobRunState,
    PipelineState,
};
use serde_json::{Value, json};
use tempfile::TempDir;

/// Set in the isolated child that runs a test body.
const CHILD_ENV: &str = "ORBIT_WORKTREE_LIFECYCLE_CHILD";
/// Upper bound on one isolated test body, Git calls included.
const CHILD_DEADLINE: Duration = orbit_common::test_env::CHILD_TEST_DEADLINE;
const BASE: &str = "agent-main";

#[cfg(unix)]
mod absorbed_candidate;
#[cfg(unix)]
mod auto_fix_conflict;
#[cfg(unix)]
mod conflict_adoption;
#[cfg(unix)]
mod declared_reclaim;
#[cfg(unix)]
mod dirty_rescue;
#[cfg(unix)]
mod generated_regeneration;
mod preparation;
mod trash;
mod unlocked_revalidation;

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

fn worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones() {
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
    let scratch = ["worktree", "add", "-b", "scratch", path_str(&hand_made)];
    git(&fixture.repo, &scratch);

    let result = gc(&host, json!({}));
    for (run, checkout) in runs.iter().zip(&checkouts) {
        let (id, report) = (run.run_id, gc_report(&result, run.run_id));
        assert_eq!(report["action"], run.expected_action, "{id}: {report:#}");
        assert_eq!(report["task_id"], run.expected_task, "{id}: {report:#}");
        let path = Path::new(report["path"].as_str().expect("report path"));
        let message = format!("{id}: gc resolves the checkout setup created");
        assert_eq!(path, checkout.path, "{message}");
        let reaped = run.expected_action == "removed";
        let gone = !checkout.path.exists();
        assert_eq!(gone, reaped, "{id}: checkout presence after gc");
        let unregistered = !registered_worktrees(&fixture.repo).contains(&checkout.path);
        assert_eq!(unregistered, reaped, "{id}: worktree registration after gc");
        if !reaped {
            let branch = branch(&checkout.path);
            let message = format!("{id}: a retained checkout keeps its branch");
            assert_eq!(branch, checkout.branch, "{message}");
        }
    }
    assert!(hand_made.exists(), "a hand-made worktree survives gc");
    assert!(registered_worktrees(&fixture.repo).contains(&canonical(&hand_made)));
}

/// [ORB-13920] A replica's GC: a settled claim licenses removal without any
/// task answer; a missing owner route and an owner transport failure are told
/// apart and each carries its reason; a directory Git does not list as a
/// worktree is kept with its remedy.
fn replica_worktree_gc_reclaims_settled_claims_and_says_why_it_keeps_the_rest() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let setup = |run_id: &str, task_id: &str| gc_checkout(&host, run_id, task_id, None);

    // The owner would not answer about this task; its settled claim
    // is enough.
    let settled = setup("jrun-claim-settled", "T-CLAIM-SETTLED");
    let settlement = "claim settled with its owner hm_owner/ws";
    host.settle_claim("jrun-claim-settled", settlement);
    let asked = WorktreeGcTaskLookup::OwnerUnreachable("asked anyway".into());
    host.answer("T-CLAIM-SETTLED", asked);
    let no_route = setup("jrun-claim-no-route", "T-CLAIM-NO-ROUTE");
    let missing = WorktreeGcTaskLookup::NoOwnerRoute("no federated owner route".into());
    host.answer("T-CLAIM-NO-ROUTE", missing);
    let unreachable = setup("jrun-claim-unreachable", "T-CLAIM-UNREACHABLE");
    let timed_out = WorktreeGcTaskLookup::OwnerUnreachable("ssh: Connection timed out".into());
    host.answer("T-CLAIM-UNREACHABLE", timed_out);
    let owner_error = setup("jrun-claim-owner-error", "T-CLAIM-OWNER-ERROR");
    let busy = "hm_owner/ws: remote tool failed (execution_failed): store busy";
    let busy = WorktreeGcTaskLookup::OwnerLookupFailed(busy.into());
    host.answer("T-CLAIM-OWNER-ERROR", busy);
    // A settled task's checkout that Git no longer lists but whose
    // `.git` link still resolves, so `git worktree repair` could bring
    // it back. (One whose link is gone is the remains of a failed
    // removal, and GC reclaims it.)
    let moved = gc_checkout(
        &host,
        "jrun-claim-moved",
        "T-CLAIM-MOVED",
        Some(TaskStatus::Done),
    );
    let remove = ["worktree", "remove", "--force", path_str(&moved.path)];
    git(&fixture.repo, &remove);
    fs::create_dir_all(&moved.path).unwrap();
    fs::write(moved.path.join("notes.txt"), "left behind").unwrap();
    let gitdir = format!("gitdir: {}\n", fixture.repo.join(".git").display());
    fs::write(moved.path.join(".git"), gitdir).unwrap();

    for (run_id, expected_action, expected_detail) in [
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
    ] {
        let result = gc(&host, json!({"target_run_id": run_id}));
        let report = &result["reports"][0];
        assert_eq!(report["run_id"], run_id, "{result:#}");
        assert_eq!(report["action"], expected_action, "{run_id}: {report:#}");
        let detail = report["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(expected_detail),
            "{run_id}: the report says why: {report:#}"
        );
    }
    let message = "the settled claim's checkout is removed";
    assert!(!settled.path.exists(), "{message}");
    let branches = git(&fixture.repo, &["branch", "--list", &settled.branch]);
    assert!(branches.is_empty(), "and its branch");
    for kept in [&no_route, &unreachable] {
        assert!(registered_worktrees(&fixture.repo).contains(&kept.path));
    }
    assert!(owner_error.path.exists(), "an owner response error is kept");
    assert!(moved.path.join("notes.txt").exists(), "never removed by GC");
}

/// One failed owner route must not suppress lookups to another owner in the
/// same worktree GC sweep.
fn replica_worktree_gc_fences_unreachable_owners_by_route() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let left = gc_checkout(&host, "jrun-owner-a", "T-OWNER-A", None);
    let right = gc_checkout(&host, "jrun-owner-b", "T-OWNER-B", None);
    // The checkout GC visits first is the one whose owner is down.
    let a = (&left, "jrun-owner-a", "T-OWNER-A");
    let b = (&right, "jrun-owner-b", "T-OWNER-B");
    let (first, second) = if left.path < right.path {
        (a, b)
    } else {
        (b, a)
    };
    let (unreachable, unreachable_run, unreachable_task) = first;
    let (reachable, reachable_run, reachable_task) = second;
    host.set_lookup_scope(unreachable_run, "hm_down/ws-a");
    host.set_lookup_scope(reachable_run, "hm_up/ws-b");
    let down = WorktreeGcTaskLookup::OwnerUnreachable("ssh: timed out".into());
    host.answer(unreachable_task, down);
    let done = WorktreeGcTaskLookup::Found {
        status: TaskStatus::Done,
        pr_status: None,
        status_since: None,
    };
    host.answer(reachable_task, done);

    let result = gc(&host, json!({}));
    let unreachable_report = gc_report(&result, unreachable_run);
    let reachable_report = gc_report(&result, reachable_run);
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
}

/// [ORB-14099] One recorded run whose `input.run_id` sanitizes to an empty
/// string must not abort the sweep. A scoped call still classifies the run
/// it names, and an unscoped call classifies every other worktree while the
/// bad runs show up as failed report entries.
fn worktree_gc_classifies_other_worktrees_when_a_run_id_sanitizes_empty() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let in_progress = Some(TaskStatus::InProgress);
    let kept = gc_checkout(&host, "jrun-gc-kept", "T-GC-KEPT", in_progress);
    let done = Some(TaskStatus::Done);
    let reaped = gc_checkout(&host, "jrun-gc-reaped", "T-GC-REAPED", done);
    // Setup rejects these tokens. The runs are recorded anyway, which
    // is the state that used to poison every later sweep.
    for (run_id, task_id, token) in [
        ("jrun-gc-dot", "T-GC-DOT", "."),
        ("jrun-gc-marks", "T-GC-MARKS", "???"),
    ] {
        let mut input = setup_input(&[task_id], run_id);
        input["run_id"] = json!(token);
        host.add_run(job_run(run_id, JobRunState::Failed, input));
    }

    let scoped = action(
        &host,
        "worktree_gc",
        &json!({"target_run_id": "jrun-gc-kept"}),
    )
    .expect("a malformed sibling run must not fail a scoped sweep");
    let scoped_reports = scoped["reports"].as_array().expect("scoped reports");
    assert_eq!(scoped_reports.len(), 1, "{scoped:#}");
    let report = gc_report(&scoped, "jrun-gc-kept");
    let ineligible = "skipped:task_status_ineligible";
    assert_eq!(report["action"], ineligible, "{scoped:#}");
    let message = "the scoped sweep retains in-progress work";
    assert!(kept.path.exists(), "{message}");

    let result = gc(&host, json!({}));
    let report = gc_report(&result, "jrun-gc-kept");
    assert_eq!(report["action"], ineligible, "{report:#}");
    assert!(kept.path.exists(), "in-progress work stays");
    let report = gc_report(&result, "jrun-gc-reaped");
    assert_eq!(report["action"], "removed", "{report:#}");
    let message = "a settled checkout is still reclaimed";
    assert!(!reaped.path.exists(), "{message}");
    for run_id in ["jrun-gc-dot", "jrun-gc-marks"] {
        let bad = gc_report(&result, run_id);
        let action = bad["action"].as_str().expect("action");
        assert!(
            action.starts_with("failed:") && action.contains("sanitizes to an empty string"),
            "ORB-14099: a run id that cannot name a directory is a failed entry, not a sweep abort: {bad:#}"
        );
    }
}

/// [ORB-17621] Non-string target_run_id is rejected with InvalidInput naming
/// target_run_id before any deletion, leaving checkouts untouched.
fn worktree_gc_rejects_non_string_target_run_id_and_deletes_nothing() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let done = Some(TaskStatus::Done);
    let settled = gc_checkout(&host, "jrun-gc-num-settled", "T-GC-NUM-SETTLED", done);
    let other = gc_checkout(&host, "jrun-gc-num-other", "T-GC-NUM-OTHER", done);
    assert!(
        settled.path.exists(),
        "setup created checkout for settled run"
    );
    assert!(other.path.exists(), "setup created checkout for other run");

    // Numeric, boolean, array, and object target_run_id values are rejected with
    // InvalidInput naming target_run_id, and must not delete any checkout.
    for bad_target in [
        json!(42),
        json!(12345),
        json!(true),
        json!(["jrun"]),
        json!({"id": "jrun"}),
    ] {
        let err = action(&host, "worktree_gc", &json!({"target_run_id": bad_target}))
            .expect_err("non-string target_run_id must be rejected");
        let OrbitError::InvalidInput(message) = err else {
            panic!("expected InvalidInput, got: {err:?}");
        };
        let named = message.contains("target_run_id");
        assert!(named, "refusal must name target_run_id: {message}");
        let message = "refused target_run_id must not delete";
        assert!(settled.path.exists(), "{message} settled checkout");
        assert!(other.path.exists(), "{message} other checkout");
    }

    // A string target_run_id still scopes the sweep and reaps only the targeted checkout,
    // skipping scratch GC.
    let result = gc(
        &host,
        json!({"target_run_id": "jrun-gc-num-settled", "scratch_older_than_hours": 1}),
    );
    let scratch = result.get("scratch");
    assert!(scratch.is_none(), "scoped sweep must skip scratch GC");
    let reports = result["reports"].as_array().expect("gc reports");
    let message = "scoped sweep should only report on target run";
    assert_eq!(reports.len(), 1, "{message}");
    assert_eq!(reports[0]["run_id"], "jrun-gc-num-settled");
    assert_eq!(reports[0]["action"], "removed");

    assert!(!settled.path.exists(), "targeted checkout was removed");
    assert!(other.path.exists(), "untargeted checkout remains untouched");
}

/// [ORB-14101] `branch_prefix` is a Git namespace, not a path. `..` and a
/// leading `-` are refused before any checkout exists. A slash stays in the
/// branch ref and becomes one directory component, including when
/// `ORBIT_WORKTREE_ROOT` relocates the root. GC still matches that checkout.
fn worktree_setup_keeps_a_branch_prefix_checkout_under_the_worktree_root() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let prefixed = |prefix: &str, run_id: &str, task_id: &str| {
        host.add_task(task_id, TaskStatus::Backlog);
        let mut input = setup_input(&[task_id], run_id);
        input["branch_prefix"] = json!(prefix);
        (action(&host, "worktree_setup", &input), input)
    };
    // `../../../tmp/esc` joined under `.orbit/state/worktrees` would
    // land at `<repo>/tmp/esc-<run>`, outside the worktree root.
    for (prefix, run_id, task_id) in [
        ("../../../tmp/esc", "jrun-escape", "T-ESCAPE"),
        ("-hidden", "jrun-dash", "T-DASH"),
        ("///", "jrun-slashes", "T-SLASHES"),
    ] {
        let error = prefixed(prefix, run_id, task_id).0;
        let error = error.expect_err("unsafe branch_prefix is refused");
        let OrbitError::InvalidInput(message) = error else {
            panic!("refused before checkout creation, got {error}");
        };
        assert!(
            message.contains("branch_prefix") && message.contains(prefix),
            "refusal names the prefix: {message}"
        );
    }

    let admitted = host.admitted();
    assert!(admitted.is_empty(), "a refused prefix admits no task");
    let message = "ORB-14101: traversal prefix must not create <repo>/tmp";
    assert!(!fixture.repo.join("tmp").exists(), "{message}");
    let message = "a refused prefix creates no worktree root";
    assert!(!fixture.repo.join(".orbit").exists(), "{message}");
    let primary = canonical(&fixture.repo);
    for path in registered_worktrees(&fixture.repo) {
        let message = "ORB-14101: refused prefix registered an extra worktree";
        assert_eq!(canonical(&path), primary, "{message}");
    }

    let configured = fixture.root.path().join("configured-worktrees");
    let _env =
        orbit_common::test_env::scoped([("ORBIT_WORKTREE_ROOT", Some(path_str(&configured)))]);
    let checkout_root = configured.join(fixture.repo.file_name().expect("repo name"));
    let run_id = "jrun-prefix-slash";
    let (setup, input) = prefixed("team/x", run_id, "T-PREFIX");
    let checkout = Checkout::from_setup(&setup.expect("slash prefix setup"));

    let message = "checkout is one child of the configured worktree root";
    let parent = checkout.path.parent();
    assert_eq!(parent, Some(checkout_root.as_path()), "{message}");
    let name = checkout.path.file_name().and_then(|name| name.to_str());
    let message = "a slash in the prefix is a hyphen in the directory name";
    assert_eq!(name, Some("team-x-jrun-prefix-slash"), "{message}");
    let message = "the prefix must not create a nested team/ directory";
    assert!(!checkout_root.join("team").exists(), "{message}");
    let canonical_root = canonical(&checkout_root);
    let canonical_checkout = canonical(&checkout.path);
    let parent = canonical_checkout.parent().map(Path::to_path_buf);
    let message = "canonical checkout stays under the configured root";
    assert_eq!(parent, Some(canonical_root), "{message}");
    let registered = registered_worktrees(&fixture.repo);
    let contained = registered
        .iter()
        .any(|path| canonical(path) == canonical_checkout);
    assert!(contained, "git registered the contained checkout");
    assert!(
        checkout.branch.starts_with("team/x/T-PREFIX-"),
        "the branch namespace keeps the slash: {}",
        checkout.branch
    );
    assert_eq!(host.admitted(), ["T-PREFIX"]);

    host.set_status("T-PREFIX", TaskStatus::Done);
    host.add_run(job_run(run_id, JobRunState::Success, input));
    let result = gc(&host, json!({}));
    let report = gc_report(&result, run_id);
    assert_eq!(report["action"], "removed", "{report:#}");
    let path = Path::new(report["path"].as_str().expect("report path"));
    let message = "gc resolves the sanitized directory setup created";
    assert_eq!(path, checkout.path, "{message}");
    assert!(!checkout.path.exists(), "gc removes the contained checkout");
    let reports = result["reports"].as_array().expect("gc reports");
    assert!(
        reports
            .iter()
            .all(|report| report["action"] != "skipped:unrecognized"),
        "a sanitized prefix is not an unrecognized nested directory: {result:#}"
    );
}

// ---------------------------------------------------------------------------
// worktree_setup: stale-checkout refusal
// ---------------------------------------------------------------------------

fn worktree_setup_refuses_a_stale_checkout_and_leaves_it_untouched() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let (first, checkout) = setup_checkout(&host, "T-STALE", "jrun-stale");
    let input = setup_input(&["T-STALE"], "jrun-stale");
    let first_base = first["base_sha"].as_str().unwrap().to_string();

    // Re-running setup against an unchanged base reattaches the same
    // checkout: the refusal below is about the base, not about reuse.
    let again = action(&host, "worktree_setup", &input).expect("reattach at same base");
    assert_eq!(again["workspace_path"], first["workspace_path"]);
    assert_eq!(head(&checkout.path), first_base);
    assert_eq!(host.admitted(), ["T-STALE", "T-STALE"]);

    let retained = commit_file(&checkout.path, "candidate.txt", "inspect me\n");
    let second_base = commit_file(&fixture.repo, "base.txt", "v2\n");

    let error = action(&host, "worktree_setup", &input)
        .expect_err("a checkout behind the new base is refused");
    assert_stale_refusal(&error, &checkout.branch, &retained, &second_base);
    assert_eq!(host.admitted().len(), 2, "a refused setup admits no task");
    assert_eq!(head(&checkout.path), retained);
    assert_eq!(read(checkout.path.join("candidate.txt")), "inspect me\n");
    assert!(status(&checkout.path).is_empty());

    // With the checkout gone, the orphaned branch still holds the
    // unexplained history and is refused the same way.
    let remove = ["worktree", "remove", "--force", path_str(&checkout.path)];
    git(&fixture.repo, &remove);
    // A run keeps one branch identity across retries, even when only
    // the orphaned branch remains. Refusal compares commit tips.
    let error = action(&host, "worktree_setup", &input)
        .expect_err("an orphan branch behind the new base is refused");
    assert_stale_refusal(&error, &checkout.branch, &retained, &second_base);
    let message = "a refused orphan attach creates no checkout";
    assert!(!checkout.path.exists(), "{message}");
    let tip = git(&fixture.repo, &["rev-parse", &checkout.branch]);
    assert_eq!(tip, retained);
    assert_eq!(host.admitted().len(), 2);
}

/// A `branch.name_template` branch keeps one name across a run's retries,
/// even once only the orphaned branch remains, and a name the task's values
/// make unsafe is refused before any ref is created.
fn worktree_setup_names_a_templated_branch_stably_and_refuses_an_unsafe_render() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let template = "feature/{{task_id}}/{{hash}}";
    host.pr_config.lock().unwrap().conventions.branch_name =
        ConventionTemplate::parse_branch("branch.name_template", template).unwrap();
    let (first, checkout) = setup_checkout(&host, "T-BR", "jrun-branch");
    assert!(
        checkout.branch.starts_with("feature/T-BR/") && checkout.branch.len() == 21,
        "{first:#}"
    );
    let input = setup_input(&["T-BR"], "jrun-branch");
    let again = action(&host, "worktree_setup", &input).expect("reattach");
    assert_eq!(again["head_ref"], first["head_ref"]);

    let remove = ["worktree", "remove", "--force", path_str(&checkout.path)];
    git(&fixture.repo, &remove);
    let retried = action(&host, "worktree_setup", &input).expect("retry on the orphan branch");
    assert_eq!(retried["head_ref"], first["head_ref"]);

    let (other, _) = setup_checkout(&host, "T-BR2", "jrun-branch-2");
    assert_ne!(other["head_ref"], first["head_ref"]);

    // With no `jira` ref the name would start with `-`, which Git reads as
    // an option: setup refuses it and creates no branch.
    let template = "{{ext.jira}}-{{task_id}}-{{hash}}";
    host.pr_config.lock().unwrap().conventions.branch_name =
        ConventionTemplate::parse_branch("branch.name_template", template).unwrap();
    host.add_task("T-DASH", TaskStatus::Backlog);
    let error = action(
        &host,
        "worktree_setup",
        &setup_input(&["T-DASH"], "jrun-dash"),
    )
    .expect_err("a leading-dash branch is refused");
    let message = error.to_string();
    assert!(
        message.contains("branch.name_template renders '-T-DASH-")
            && message.contains("start with '-'"),
        "{message}"
    );
    let branches = git(
        &fixture.repo,
        &["for-each-ref", "--format=%(refname:short)"],
    );
    assert!(!branches.contains("T-DASH"), "{branches}");
}

/// A registered checkout whose admin `HEAD` is corrupt (a crash mid-commit)
/// cannot report its status, so setup cannot tell dirty from clean. The edits
/// in it are unrecoverable once the checkout is force-removed.
fn worktree_setup_refuses_a_checkout_with_unreadable_status_and_keeps_its_edits() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-CORRUPT", "jrun-corrupt");
    let input = setup_input(&["T-CORRUPT"], "jrun-corrupt");
    fs::write(checkout.path.join("uncommitted.txt"), "agent edit\n").unwrap();
    let admin_head = git_path(&checkout.path, "HEAD");
    fs::write(&admin_head, "not a ref\n").unwrap();
    let unreadable = crate::git_test_support::command()
        .args(["status", "--porcelain"])
        .current_dir(&checkout.path)
        .output()
        .is_ok_and(|output| !output.status.success());
    let message = "fixture: status must be unreadable for the corrupt checkout";
    assert!(unreadable, "{message}");

    let error = action(&host, "worktree_setup", &input)
        .expect_err("an unreadable-status checkout is refused, not removed");
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::Execution(_))
            && message.contains("retains work")
            && message.contains("status=unreadable"),
        "refusal carries the unreadable-status evidence: {message}"
    );
    assert_eq!(host.admitted().len(), 1, "a refused setup admits no task");
    let edit = read(checkout.path.join("uncommitted.txt"));
    let message = "the uncommitted edit survives the refused setup";
    assert_eq!(edit, "agent edit\n", "{message}");
    let message = "the corrupt admin state is left for inspection";
    assert_eq!(read(&admin_head), "not a ref\n", "{message}");
}

/// `origin/main` and `main` name one local landing branch. Setup's pre-check
/// and `merge_batch_worktree_into_base` both inspect the linked checkout that
/// holds it, including when the primary checkout is a different dirty tree.
fn local_landing_precheck_uses_the_normalized_base_checkout() {
    let fixture = Fixture::new();
    git(&fixture.repo, &["branch", "main"]);
    let requested = fixture.root.path().join("landing");
    let add = ["worktree", "add", path_str(&requested), "main"];
    git(&fixture.repo, &add);
    let landing = checkout_holding(&fixture.repo, "main");
    let host = LifecycleHost::new(&fixture.repo);
    host.add_task("T-LAND", TaskStatus::Backlog);
    let checkouts_before = registered_worktrees(&fixture.repo);

    let original = fs::read(landing.join("README.md")).unwrap();
    fs::write(landing.join("README.md"), "uncommitted landing work\n").unwrap();
    for (field, spelling) in [
        ("base", "main"),
        ("base", "origin/main"),
        ("base_branch", "origin/main"),
    ] {
        let input = landing_input(field, spelling, "jrun-land-refuse");
        let error = action(&host, "worktree_setup", &input)
            .expect_err("a dirty landing checkout is refused before admission");
        assert_landing_checkout_refusal(&error, &landing, spelling);
    }
    let admitted = host.admitted();
    assert!(admitted.is_empty(), "a refused pre-check admits no task");
    let checkouts = registered_worktrees(&fixture.repo);
    let message = "a refused pre-check creates no worktree";
    assert_eq!(checkouts, checkouts_before, "{message}");

    fs::write(landing.join("README.md"), &original).unwrap();
    fs::write(fixture.repo.join("unrelated.txt"), "primary dirt\n").unwrap();
    let mut workspace = None;
    for (spelling, run_id) in [
        ("main", "jrun-land-main"),
        ("origin/main", "jrun-land-origin"),
    ] {
        let input = landing_input("base", spelling, run_id);
        let output = action(&host, "worktree_setup", &input).unwrap_or_else(|error| {
            panic!(
                "base spelling {spelling} must admit when only the primary checkout is dirty: {error}"
            )
        });
        workspace = Some((run_id, Checkout::from_setup(&output).path));
    }
    let (run_id, workspace) = workspace.expect("admitted checkout");
    let message = "setup leaves the primary checkout on its own branch";
    assert_eq!(branch(&fixture.repo), BASE, "{message}");

    fs::write(landing.join("README.md"), "uncommitted landing work\n").unwrap();
    for spelling in ["main", "origin/main"] {
        let error = merge(&host, run_id, spelling, &workspace)
            .expect_err("merge refuses the same dirty landing checkout");
        assert_landing_checkout_refusal(&error, &landing, spelling);
    }

    fs::write(landing.join("README.md"), &original).unwrap();
    for spelling in ["main", "origin/main"] {
        let merged = merge(&host, run_id, spelling, &workspace).unwrap_or_else(|error| {
            panic!(
                "base spelling {spelling} must merge into the clean linked checkout while the primary stays dirty: {error}"
            )
        });
        assert_eq!(merged["base"], "main");
    }
    assert_eq!(branch(&landing), "main");
    assert_eq!(branch(&fixture.repo), BASE);
    assert_eq!(read(fixture.repo.join("unrelated.txt")), "primary dirt\n");
}

// ---------------------------------------------------------------------------
// Local delivery: operator-owned untracked files in the landing checkout
// ---------------------------------------------------------------------------

fn local_landing_preserves_unrelated_untracked_files() {
    for linked in [false, true] {
        let fixture = Fixture::new();
        git(&fixture.repo, &["branch", "main"]);
        if linked {
            let landing = fixture.root.path().join("landing");
            let add = ["worktree", "add", path_str(&landing), "main"];
            git(&fixture.repo, &add);
        } else {
            git(&fixture.repo, &["checkout", "main"]);
        }
        let landing = checkout_holding(&fixture.repo, "main");
        let host = LifecycleHost::new(&fixture.repo);
        let notes = ["on-call-goal.md", "operations/research/notes\n\".md"];
        let bytes = b"operator notes\n\0\xff\r\n";
        for note in notes {
            let path = landing.join(note);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let run_id = "jrun-untracked-notes";
        let checkout = landing_checkout(&host, "main", run_id);
        let feature = "operations/scripts/feature.txt";
        commit_tracked_record(&checkout.path, feature, "candidate\n");
        let candidate = head(&checkout.path);

        merge(&host, run_id, "main", &checkout.path)
            .expect("local delivery lands beside unrelated operator notes");

        assert_eq!(head(&landing), candidate);
        assert_eq!(fs::read(landing.join(feature)).unwrap(), b"candidate\n");
        for note in notes {
            let landed = fs::read(landing.join(note)).unwrap();
            assert_eq!(landed, bytes, "operator bytes survive: {note:?}");
        }
        let others = ["ls-files", "--others", "--exclude-standard", "-z"];
        let untracked = git(&landing, &others);
        assert_eq!(untracked, format!("{}\0", notes.join("\0")));
    }
}

fn local_landing_names_only_untracked_paths_conflicting_with_the_candidate() {
    // Exact paths, newline/quote spelling, file/directory collisions,
    // and a path created after the merge action's initial pre-check.
    for (incoming, untracked, at_setup, at_intent) in [
        ("candidate.txt", "candidate.txt", true, false),
        ("line\n\".txt", "line\n\".txt", false, false),
        ("folder/child.txt", "folder", false, false),
        ("folder", "folder/child.txt", false, false),
        ("late.txt", "late.txt", false, true),
    ] {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        let operator_path = fixture.repo.join(untracked);
        let operator_bytes = b"operator-owned\0\xff\n";
        let write_operator = || {
            fs::create_dir_all(operator_path.parent().unwrap()).unwrap();
            fs::write(&operator_path, operator_bytes).unwrap();
        };
        fs::write(fixture.repo.join("unrelated.txt"), "keep me\n").unwrap();
        if at_setup {
            write_operator();
        }
        let run_id = "jrun-untracked-conflict";
        // Setup cannot yet know the candidate's paths.
        let checkout = landing_checkout(&host, BASE, run_id);
        commit_tracked_record(&checkout.path, incoming, "candidate\n");
        if at_intent {
            let write = (operator_path.clone(), operator_bytes.to_vec());
            *host.landing_write.lock().unwrap() = Some(write);
        } else if !at_setup {
            write_operator();
        }
        let before = head(&fixture.repo);
        let candidate = head(&checkout.path);
        let error = merge(&host, run_id, BASE, &checkout.path)
            .expect_err("a conflicting operator path must refuse before merge");
        let message = error.to_string();
        assert!(
            message.contains("untracked paths conflicting with the incoming changes"),
            "{message}"
        );
        assert!(
            message.contains(&format!("\n{untracked:?}\n")),
            "names the exact conflicting path: {message}"
        );
        assert!(
            !message.contains("unrelated.txt"),
            "unrelated notes are absent from the refusal: {message}"
        );
        assert_eq!(head(&fixture.repo), before);
        assert_eq!(head(&checkout.path), candidate);
        assert_eq!(fs::read(&operator_path).unwrap(), operator_bytes);
        assert_eq!(read(fixture.repo.join("unrelated.txt")), "keep me\n");
    }
}

fn local_landing_still_refuses_tracked_and_unmerged_work_at_setup_and_merge() {
    for dirty_kind in ["unstaged", "staged", "unmerged"] {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        let run_id = "jrun-tracked-work";
        // Set up on a clean base.
        let checkout = landing_checkout(&host, BASE, run_id);
        commit_file(&checkout.path, "candidate.txt", "candidate\n");
        if dirty_kind == "unmerged" {
            git(&fixture.repo, &["checkout", "-b", "other"]);
            commit_file(&fixture.repo, "README.md", "other side\n");
            git(&fixture.repo, &["checkout", BASE]);
            commit_file(&fixture.repo, "README.md", "base side\n");
            let merge = crate::git_test_support::command()
                .args(["merge", "other"])
                .current_dir(&fixture.repo)
                .output()
                .unwrap();
            let message = "fixture creates an actual merge conflict";
            assert!(!merge.status.success(), "{message}");
            assert!(!git(&fixture.repo, &["ls-files", "-u"]).is_empty());
        } else {
            fs::write(fixture.repo.join("README.md"), "operator changes\n").unwrap();
            if dirty_kind == "staged" {
                git(&fixture.repo, &["add", "README.md"]);
            }
        }
        fs::write(fixture.repo.join("unrelated.txt"), "notes\n").unwrap();
        let before = head(&fixture.repo);
        let primary_status = status(&fixture.repo);
        let contents = fs::read(fixture.repo.join("README.md")).unwrap();
        let checkouts = registered_worktrees(&fixture.repo);
        let refused_setup = landing_input("base", BASE, "jrun-refused-tracked");
        for (name, input) in [
            ("worktree_setup", refused_setup),
            ("git_merge", merge_input(run_id, BASE, &checkout.path)),
        ] {
            let error = action(&host, name, &input)
                .expect_err("tracked/unmerged work refuses local delivery");
            let message = error.to_string();
            let expected = if dirty_kind == "unmerged" {
                "has unresolved merge conflicts:"
            } else {
                "must be clean before merge_batch_worktree_into_base"
            };
            assert!(
                message.contains(expected) && message.contains("README.md"),
                "{dirty_kind}: {message}"
            );
            assert!(
                !message.contains("unrelated.txt"),
                "only hazardous paths are named: {message}"
            );
            assert_eq!(head(&fixture.repo), before);
            assert_eq!(status(&fixture.repo), primary_status);
            assert_eq!(fs::read(fixture.repo.join("README.md")).unwrap(), contents);
            assert_eq!(registered_worktrees(&fixture.repo), checkouts);
            assert_eq!(host.admitted(), ["T-LAND"]);
        }
    }
}

// ---------------------------------------------------------------------------
// candidate_resume: a requeued task resumes its preserved candidate
// ---------------------------------------------------------------------------

/// A clean completed candidate is carried to the exact final gate without
/// preliminary validation. Count native command executions and measure only
/// the resume-to-final-gate path, excluding compilation and fixture setup.
#[allow(clippy::print_stderr)]
fn a_validated_candidate_is_resumed_on_the_new_base_without_implementation() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let base = preserved.advance_base();
    let trace = preserved.fixture.root.path().join("gate-trace");
    let probes = [
        "test -f feature.txt",
        "read line < base.txt && test \"$line\" = v2",
        "git diff --check",
        "git cat-file -e HEAD:base.txt",
    ];
    let commands = probes.iter().enumerate().map(|(index, probe)| {
        format!(
            "{probe} && printf '{index} %s %s\\n' \"$(git rev-parse HEAD)\" \"$(git status --porcelain | wc -l | tr -d ' ')\" >> '{}'",
            trace.display().to_string().replace('\'', "'\\''")
        )
    }).collect::<Vec<_>>();
    preserved.require(&commands.iter().map(String::as_str).collect::<Vec<_>>());
    let setup = preserved.next_setup();
    assert_eq!(setup["base_sha"], base.as_str());
    let start = Instant::now();
    let resumed = preserved.resume(&setup).expect("candidate_resume");
    let resume_elapsed = start.elapsed();
    let resume_calls = fs::read_to_string(&trace).unwrap_or_default();
    let resume_calls = resume_calls.lines().count();
    assert_eq!(resumed["implement"], false);
    assert_eq!(resumed["source_run_id"], FAILED_RUN);
    assert_eq!(resumed["source_sha"], preserved.candidate.as_str());
    assert_eq!(resumed["repair"], Value::Null);
    let checkout = workspace(&setup);
    assert_eq!(head(&checkout), base);
    assert_eq!(status(&checkout), "?? feature.txt");
    let committed = preserved.commit(&setup);
    let head_sha = committed["commit_sha"].as_str().unwrap();
    let parent = git(&checkout, &["rev-parse", &format!("{head_sha}^")]);
    assert_eq!(parent, base);
    let feature = git(&checkout, &["show", &format!("{head_sha}:feature.txt")]);
    assert_eq!(feature, "feature");

    // The base moves after commit; validate the synchronized head,
    // rather than the preserved source or the first commit.
    let synchronized_base = commit_file(&preserved.fixture.repo, "synced.txt", "v3\n");
    let common = json!({
        "job_run_id": NEXT_RUN, "completed_task_ids": [RESUME_TASK],
        "workspace_path": checkout, "base": BASE, "base_sync": "local",
    });
    let prepared = action(&preserved.host, "pr_prepare", &common).unwrap();
    let mut sync_input = prepared.as_object().unwrap().clone();
    sync_input.extend(common.as_object().unwrap().clone());
    let synced = action(&preserved.host, "git_rebase", &Value::Object(sync_input)).unwrap();
    assert_eq!(synced["base_sha"], synchronized_base);
    let final_head = head(&checkout);
    assert_ne!(final_head, head_sha);
    let gate_start = Instant::now();
    let mut gate_input = setup.clone();
    gate_input["base_sha"] = synced["base_sha"].clone();
    let validated = preserved.validate(&gate_input).expect("final gate");
    let gate_elapsed = gate_start.elapsed();
    let trace = read(&trace);
    let calls = trace.lines().count();
    eprintln!(
        "resume_gate_measurement {}",
        json!({
            "outcome": resumed["outcome"], "commands": commands,
            "resume_command_calls": resume_calls, "final_command_calls": calls - resume_calls,
            "full_gate_passes": calls / probes.len(),
            "resume_ms": resume_elapsed.as_secs_f64() * 1000.0,
            "final_gate_ms": gate_elapsed.as_secs_f64() * 1000.0,
            "critical_path_ms": start.elapsed().as_secs_f64() * 1000.0,
            "preserved_source": preserved.candidate, "committed_head": head_sha,
            "synchronized_base": synchronized_base, "tested_head": validated["tested_head"],
            "trace": trace,
        })
    );
    // This is the regression assertion; before the fix the same
    // native probes run four times here and four times at the gate.
    assert_eq!(resume_calls, 0, "resume must not run a duplicate full gate");
    assert_eq!(calls, probes.len(), "exactly one full gate");
    for (index, line) in trace.lines().enumerate() {
        let message = "each gate command sees the clean synchronized head";
        assert_eq!(line, format!("{index} {final_head} 0"), "{message}");
    }
    assert_eq!(validated["tested_head"], final_head);
    assert_eq!(resumed["outcome"], "resumed_carried", "{resumed}");
    assert_resume_recorded(&preserved, "resumed_carried");
}

/// A candidate an Orbit upgrade held when it refused the reviewer's `orbit`
/// is resumed by the task's next run, as carried work: the upgrade never
/// judged it.
fn a_candidate_held_for_an_upgrade_refusal_is_resumed_by_the_next_run() {
    assert_held_candidate_resumes_carried("review", "held_upgrade_pending");
}

/// A candidate held when its parent stopped waiting for the run [ORB-15643]
/// is resumed by the task's requeued run: the cancel judged nothing.
fn a_candidate_held_for_a_wait_timeout_is_resumed_by_the_next_run() {
    assert_held_candidate_resumes_carried("validate", "held_child_timeout");
}

/// The task's next run resumes a candidate whose failed run's handoff at
/// `failed_step_id` recorded `decision`, as carried work.
fn assert_held_candidate_resumes_carried(failed_step_id: &str, decision: &str) {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    preserved.preserve_decision(failed_step_id, decision);
    preserved.require(&["test -f feature.txt"]);
    let (_, resumed) = preserved.resumed();
    assert_resumed(decision, &resumed, "resumed_carried", false);
    let source = preserved.candidate.as_str();
    assert_eq!(resumed["source_sha"], source, "{decision}");
    assert_resume_recorded(&preserved, "resumed_carried");
}

/// A candidate that conflicts with the advanced base is handed to the
/// implementer as uncommitted work with the conflict markers and paths;
/// validation does not run on a conflicted tree.
fn a_conflicting_candidate_is_handed_to_the_implementer_with_its_conflict() {
    let preserved = PreservedCandidate::new("base.txt", "candidate\n");
    let base = preserved.advance_base();
    preserved.require(&["exit 99"]);
    let (setup, resumed) = preserved.resumed();
    assert_resumed("conflict", &resumed, "resumed_repaired", true);
    assert_eq!(resumed["repair"]["trigger"], "conflict");
    assert_eq!(resumed["repair"]["conflicting_paths"], json!(["base.txt"]));
    let output = resumed["repair"]["output"].as_str().unwrap();
    assert!(output.contains("base.txt"), "{resumed}");
    let checkout = workspace(&setup);
    assert_eq!(head(&checkout), base);
    assert_both_sides_of_the_conflict(&checkout);
    let unmerged = git(&checkout, &["diff", "--name-only", "--diff-filter=U"]);
    assert!(unmerged.is_empty(), "no merge state is left behind");
    assert_resume_recorded(&preserved, "resumed_repaired");
}

/// A carried candidate is judged at the final gate; a candidate failure
/// retains the failing command and output for existing recovery.
fn a_candidate_failing_validation_is_handed_to_the_implementer_with_the_output() {
    for (case, command) in [
        (
            "base-passes",
            "test ! -f feature.txt || { echo 'feature.txt is wrong' >&2; exit 3; }",
        ),
        (
            "disjoint",
            "if test -f feature.txt; then echo 'test suite::added ... FAILED'; else echo 'test suite::broken ... FAILED'; fi; exit 3",
        ),
        (
            "shared-plus-new",
            "echo 'test suite::broken ... FAILED'; test ! -f feature.txt || echo 'test suite::added ... FAILED'; exit 3",
        ),
        ("unknown", "echo 'lint is red' >&2; exit 3"),
    ] {
        let (preserved, setup, resumed) = PreservedCandidate::committed(command);
        assert_resumed(case, &resumed, "resumed_carried", false);
        let error = preserved.validate(&setup);
        let message = error.expect_err("final gate rejects candidate").to_string();
        let baseline_red = orbit_types::workflow::is_baseline_red_failure(None, Some(&message));
        assert!(!baseline_red, "{case}: {message}");
        let environment =
            orbit_types::workflow::is_validation_environment_failure(None, Some(&message));
        assert!(!environment, "{case}: {message}");
        let log = preserved.validation_log();
        assert_eq!(log["command"], command, "{case}");
        assert_eq!(log["exit_code"], 3, "{case}");
        assert_eq!(log["baseline_reproduces"], false, "{case}");
        let expected = match case {
            "base-passes" => "feature.txt is wrong",
            "unknown" => "lint is red",
            _ => "suite::added",
        };
        let output = log["output"].as_str().unwrap();
        assert!(output.contains(expected), "{case}: {resumed}");
        if case == "disjoint" || case == "shared-plus-new" {
            let baseline = &log["baseline"]["failures"];
            assert_eq!(baseline, &json!(["test suite::broken"]), "{case}");
            let failures = log["failures"].as_array().unwrap();
            assert!(failures.contains(&json!("test suite::added")), "{case}");
        }
        let feature = read(workspace(&setup).join("feature.txt"));
        let message = "the implementer starts from the candidate";
        assert_eq!(feature, "feature\n", "{case}: {message}");
        assert_resume_recorded(&preserved, "resumed_carried");
    }
}

/// A candidate whose required command fails on the base exactly as on the
/// candidate is not handed to the implementer: no repair of the candidate
/// can make it pass, so only the final gate produces the typed hold.
fn a_candidate_failing_validation_its_base_shares_is_not_handed_to_the_implementer() {
    for (kind, command) in [
        (
            "baseline",
            "printf 'error: lint is red\\n  --> src/lib.rs:1:1\\n' >&2; exit 3",
        ),
        ("environment", "orbit-resume-fixture-missing-tool"),
        ("signal", "kill -TERM $$"),
    ] {
        let (preserved, setup, resumed) = PreservedCandidate::committed(command);
        assert_resumed(kind, &resumed, "resumed_carried", false);
        assert!(resumed["repair"].is_null(), "{kind}: {resumed}");
        let error = preserved.validate(&setup).expect_err("final gate refuses");
        let message = error.to_string();
        let baseline_red = orbit_types::workflow::is_baseline_red_failure(None, Some(&message));
        assert_eq!(baseline_red, kind == "baseline", "{kind}: {message}");
        let environment =
            orbit_types::workflow::is_validation_environment_failure(None, Some(&message));
        assert_eq!(environment, kind != "baseline", "{kind}: {message}");
        let feature = git(&workspace(&setup), &["show", "HEAD:feature.txt"]);
        assert_eq!(feature, "feature", "{kind}");
        assert_resume_recorded(&preserved, "resumed_carried");
    }
}

/// A handoff whose failed step is the implementation, or any step before
/// `commit`, is unfinished. Requeue applies that candidate and runs the
/// implementer even when owner validation would pass, and when no command
/// is configured. A failure at `commit` still resumes as carried.
fn an_implementation_failure_handoff_is_requeued_for_the_implementer() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    // Passing checks are the case that used to skip the implementer.
    preserved.require(&["test -s feature.txt"]);
    // Empty required commands used to accept unfinished work
    // without running anything. The nested implement step and the
    // steps before it share the boundary.
    for (step, run_id, commands) in [
        ("implement_bundle", "jrun-implement-bundle", None),
        ("implement_one", "jrun-implement-one", Some(&[][..])),
        ("review_preflight", "jrun-review-preflight", None),
        ("worktree", "jrun-worktree", None),
        ("resume_candidate", "jrun-resume-candidate", None),
    ] {
        if let Some(commands) = commands {
            preserved.require(commands);
        }
        let (setup, resumed) = preserved.resume_failed_step(step, run_id);
        let resumed = resumed.unwrap_or_else(|error| panic!("{step}: {error}"));
        assert_resumed(step, &resumed, "resumed_repaired", true);
        assert_eq!(resumed["repair"]["trigger"], "implementation", "{step}");
        assert_eq!(resumed["repair"]["failed_step_id"], step, "{step}");
        if step == "implement_bundle" {
            let feature = read(workspace(&setup).join("feature.txt"));
            let message = "the implementer starts from the partial candidate";
            assert_eq!(feature, "feature\n", "{message}");
            assert_resume_recorded(&preserved, "resumed_repaired");
        }
    }

    // `commit` is the first step that preserved a finished implementation.
    preserved.require(&["test -s feature.txt"]);
    let committed = preserved.resume_failed_step("commit", "jrun-commit").1;
    let committed = committed.expect("commit handoff");
    assert_resumed("commit", &committed, "resumed_carried", false);
    assert_resume_recorded(&preserved, "resumed_carried");
}

/// [ORB-15548] Carried candidates still owe host and replay coverage at the
/// final gate, including known-red commands. This fixture follows the
/// `ORBIT_VALIDATION_SELECTION` replay request and reports executed identities:
/// production correctly refuses a base run with a different selection or
/// without proof that the requested test ran.
fn resumed_known_red_candidates_must_complete_host_and_replay_coverage() {
    let notice =
        "DEFERRED: bubblewrap unavailable: fixture: No permissions to create new namespace";
    for (case, output_notice, replay, tests_run, validated) in [
        ("executed", "", false, 3, true),
        ("deferred", notice, false, 3, false),
        ("skipped", "SKIP: sandbox fixture", false, 3, false),
        ("replay-zero", "", true, 0, false),
        ("replay-executed", "", true, 3, true),
        ("replay-deferred", notice, true, 3, false),
        ("replay-missing", "", true, 3, false),
        ("replay-partial", "", true, 3, false),
        ("selection-mismatch", "", true, 3, false),
    ] {
        let preserved = PreservedCandidate::new("feature.txt", "feature\n");
        let mismatch_selection = if case == "selection-mismatch" {
            "if [ -n \"$ORBIT_VALIDATION_SELECTION\" ]; then \
             selection='{\"packages\":[\"other\"],\"failing_tests\":[\"fixture\"]}'; fi; "
        } else {
            ""
        };
        let executed_tests = match case {
            "replay-missing" => "",
            "replay-partial" => ",\"executed_tests\":[\"other\"]",
            "replay-zero" => ",\"executed_tests\":[]",
            _ => ",\"executed_tests\":[\"fixture\"]",
        };
        let command = format!(
            "selection=\"$ORBIT_VALIDATION_SELECTION\"; \
             [ -n \"$selection\" ] || selection='{{\"packages\":[\"fixture\"]}}'; \
             {mismatch_selection} \
             printf 'test fixture ... FAILED\\n{output_notice}\\n'; \
             printf '{{\"schema_version\":1,\"selection\":%s,\"tests_run\":{tests_run}{executed_tests}}}' \
             \"$selection\" > \"$ORBIT_VALIDATION_SUMMARY\"; exit 2"
        );
        preserved.require(&[&command]);
        if replay {
            let deferred = json!({
                "schema_version": 1,
                "run_id": FAILED_RUN,
                "deferred": {
                    "command": command,
                    "exit_code": 0,
                    "tests_run": 3,
                    "base": git(&preserved.fixture.repo, &["rev-parse", BASE]),
                    "notices": [notice],
                    "probe": {
                        "command": "bwrap --unshare-user --ro-bind / / -- /bin/true",
                        "exit_code": 1,
                        "output": "bwrap: No permissions to create new namespace",
                    },
                },
            });
            let path = orbit_types::workflow::DEFERRED_SANDBOX_ARTIFACT;
            preserved
                .host
                .set_artifact(RESUME_TASK, json_artifact(path, &deferred));
        }
        let (setup, resumed) = preserved.resumed();
        assert_resumed(case, &resumed, "resumed_carried", false);
        preserved.commit(&setup);
        let verdict = preserved.validate(&setup);
        if validated {
            let decision = &verdict.unwrap()["decision"];
            assert_eq!(decision, "passed_with_known_red", "{case}");
            continue;
        }
        let error = verdict.expect_err("unexecuted coverage refuses");
        let message = error.to_string();
        let refusal = match case {
            "replay-zero" => "replayed nothing",
            "replay-missing" => "no executed-test identity proof",
            "replay-partial" => "did not execute requested test",
            "selection-mismatch" => "the base run tested",
            _ => "",
        };
        if case == "replay-zero" {
            let denied = matches!(error, OrbitError::PolicyDenied(_));
            assert!(denied, "zero replay is refused");
        }
        if !refusal.is_empty() {
            assert!(message.contains(refusal), "{case}: {message}");
        } else {
            assert!(
                orbit_types::workflow::is_baseline_red_failure(None, Some(&message)),
                "{case}: incomplete coverage cannot be known red: {message}"
            );
        }
    }
}

/// A spec change since the candidate's run, or an operator discard recorded
/// since that run began, forces a fresh implementation with the reason, and
/// leaves the checkout untouched. A discard older than the run does not.
fn a_changed_spec_or_an_operator_discard_implements_fresh() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let base = preserved.advance_base();
    let setup = preserved.next_setup();
    let checkout = workspace(&setup);
    let fresh = |case: &str, reason: &str| {
        let resumed = preserved.resume(&setup).expect("candidate_resume");
        assert_eq!(resumed["outcome"], "fresh", "{case}: {resumed}");
        let why = resumed["reason"].as_str().unwrap();
        assert!(why.contains(reason), "{case}: {resumed}");
        assert_eq!(head(&checkout), base, "{case}");
        assert!(status(&checkout).is_empty(), "{case}");
        assert_resume_recorded(&preserved, "fresh");
        resumed
    };

    preserved
        .host
        .set_description(RESUME_TASK, "A re-scoped task.");
    let changed = fresh("spec", "changed since");
    assert_eq!(changed["implement"], true);
    assert_eq!(changed["source_sha"], preserved.candidate.as_str());

    preserved.host.set_description(RESUME_TASK, "");
    preserved.host.clear_history(RESUME_TASK);
    preserved
        .host
        .record_history(RESUME_TASK, discard_entry(Utc::now()));
    fresh("discard", "discarded");

    preserved.host.clear_history(RESUME_TASK);
    let earlier = discard_entry(Utc::now() - chrono::Duration::hours(1));
    preserved.host.record_history(RESUME_TASK, earlier);
    let resumed = preserved.resume(&setup).expect("candidate_resume");
    assert_eq!(
        resumed["outcome"], "resumed_carried",
        "a discard from before the candidate's run does not apply: {resumed}"
    );
}

/// [ORB-14450] Selectors are preparation hints, not what the task means: a
/// task-pilot or operator edit to them after a failure preserved the
/// candidate keeps it, while a description or criteria edit still does not.
fn a_selector_only_edit_keeps_the_preserved_candidate() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let selectors = ["file:feature.txt", "dir:docs"];
    preserved.host.set_context_files(RESUME_TASK, &selectors);
    let (_, resumed) = preserved.resumed();
    assert_resumed("selectors", &resumed, "resumed_carried", false);
    assert_resume_recorded(&preserved, "resumed_carried");

    let criteria = ["A new criterion."];
    preserved
        .host
        .set_acceptance_criteria(RESUME_TASK, &criteria);
    let changed = preserved.requeue("jrun-criteria").1;
    let changed = changed.expect("candidate_resume");
    assert_eq!(changed["outcome"], "fresh", "{changed}");
    assert_eq!(changed["reason_code"], "spec_changed", "{changed}");
}

/// [ORB-14450] A run held on named external evidence ends before the
/// failure handoff and records no checkpoint. Once the evidence receipt for
/// that run is the task's latest decision, the next run resumes the exact
/// commit the hold names: no implementer, and no owner validation here — the
/// pipeline's own validation and the fresh review judge it. A selector edit
/// since the hold does not retire it.
fn an_evidence_receipt_resumes_the_held_candidate_without_the_implementer() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    preserved.hold(&preserved.candidate);
    let selectors = ["file:feature.txt", "dir:docs"];
    preserved.host.set_context_files(RESUME_TASK, &selectors);
    // A validated resume would hand this red command's candidate to
    // the implementer.
    preserved.require(&["exit 3"]);
    let (setup, resumed) = preserved.resumed();
    assert_resumed("held", &resumed, "resumed_held", false);
    assert_eq!(resumed["repair"], Value::Null);
    assert_eq!(resumed["source_run_id"], FAILED_RUN);
    assert_eq!(resumed["source_sha"], preserved.candidate.as_str());
    assert_eq!(resumed["source_branch"], preserved.branch.as_str());
    let message = "the held candidate is applied as uncommitted work";
    assert_eq!(status(&workspace(&setup)), "?? feature.txt", "{message}");
    assert_resume_recorded(&preserved, "resumed_held");
}

/// [ORB-14450] A held candidate that is gone or retired implements fresh
/// with a typed reason and an untouched checkout: its commit is missing, an
/// operator discarded it, the description changed, or the receipt is not
/// the task's latest decision.
fn a_held_candidate_that_is_gone_or_retired_implements_fresh_with_a_typed_reason() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let base = preserved.advance_base();
    let setup = preserved.next_setup();
    let checkout = workspace(&setup);
    let fresh = |code: &str| {
        let resumed = preserved.resume(&setup).expect("candidate_resume");
        assert_resumed(code, &resumed, "fresh", true);
        assert_eq!(resumed["reason_code"], code, "{resumed}");
        assert_eq!(head(&checkout), base, "{code}");
        assert!(status(&checkout).is_empty(), "{code}");
    };

    let missing = "0123456789abcdef0123456789abcdef01234567";
    preserved.hold(missing);
    fresh("candidate_missing");
    assert_resume_recorded_for(&preserved, "fresh", missing);

    preserved.hold(&preserved.candidate);
    preserved
        .host
        .record_history(RESUME_TASK, discard_entry(Utc::now()));
    fresh("candidate_discarded");

    preserved.hold(&preserved.candidate);
    preserved
        .host
        .set_description(RESUME_TASK, "A re-scoped task.");
    fresh("spec_changed");
    preserved.host.set_description(RESUME_TASK, "");

    preserved.hold(&preserved.candidate);
    let blocked = TaskHistoryEntry {
        from_status: Some(TaskStatus::Backlog),
        to_status: Some(TaskStatus::Blocked),
        ..history_entry(Utc::now(), "human:operator", "status_changed")
    };
    preserved.host.record_history(RESUME_TASK, blocked);
    fresh("no_candidate");
}

/// [ORB-14257] A claimed PR leaf resumes the candidate its owner kept from
/// the task's last claim, handed in as `candidate`: applied onto the new
/// base for the implementer to continue, never validated here and never
/// written to a task history that lives on the owner. Without one, or when
/// its changes conflict with the base, the usual outcomes apply.
fn a_claimed_leaf_continues_the_candidate_its_owner_kept() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let base = preserved.advance_base();
    // Would refuse the candidate if validation ran here.
    preserved.require(&["exit 99"]);
    preserved.host.clear_history(RESUME_TASK);
    let claimed = |setup: &Value, candidate: Value| {
        let resumed = preserved.resume_claimed(setup, json!({"candidate": candidate}));
        resumed.expect("candidate_resume")
    };
    let kept = |failed_step_id: &str| {
        json!({
            "branch": preserved.branch,
            "head_sha": preserved.candidate,
            "source_run_id": "jrun-earlier-claim",
            "failed_step_id": failed_step_id,
        })
    };

    let setup = preserved.next_setup();
    let resumed = claimed(&setup, kept("sync_base"));
    assert_resumed("kept", &resumed, "resumed_repaired", true);
    assert_eq!(resumed["repair"]["trigger"], "continuation", "{resumed}");
    assert_eq!(resumed["repair"]["failed_step_id"], "sync_base");
    assert_eq!(resumed["source_run_id"], "jrun-earlier-claim");
    assert_eq!(resumed["source_sha"], preserved.candidate.as_str());
    let checkout = workspace(&setup);
    assert_eq!(head(&checkout), base);
    let message = "the kept candidate is applied as uncommitted work";
    assert_eq!(status(&checkout), "?? feature.txt", "{message}");
    let history = preserved.host.history(RESUME_TASK);
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "candidate_resume"),
        "a claimed leaf writes no task history"
    );

    let review = preserved.relinked_setup("jrun-claimed-review");
    let refused = claimed(&review, kept("review_gate_settle"));
    assert_eq!(refused["repair"]["trigger"], "review", "{refused}");

    let fresh = preserved.relinked_setup("jrun-claimed-fresh");
    let none = claimed(&fresh, Value::Null);
    assert_resumed("no candidate", &none, "fresh", true);
    let message = "nothing is applied without a candidate";
    assert!(status(&workspace(&fresh)).is_empty(), "{message}");
}

/// [ORB-14603] A claim's leaf this machine executed itself leaves no failure
/// handoff on its run; the owner's next run of the task continues the
/// candidate kept from that leaf's claim, naming the claim and machine in the
/// task's history. A candidate kept from some other run is not resurrected.
fn a_local_claim_leaf_falls_back_to_the_candidate_its_claim_kept() {
    let preserved = PreservedCandidate::new("feature.txt", "feature\n");
    let base = preserved.advance_base();
    preserved.host.run_states.lock().unwrap().remove(FAILED_RUN);
    *preserved.host.machine.lock().unwrap() = Some("hm_local".to_string());
    let kept = |source_run_id: &str| KeptClaimCandidate {
        claim_id: "claim-1".to_string(),
        machine_id: "hm_local".to_string(),
        candidate: ClaimCandidateRef {
            branch: preserved.branch.clone(),
            head_sha: preserved.candidate.clone(),
            pull_request: None,
            pull_request_head: None,
            source_run_id: Some(source_run_id.to_string()),
            failed_step_id: Some("sync_base".to_string()),
            published: false,
            durable_ref: None,
            carry_failure: None,
            interrupted_review: None,
            landing_stop: None,
        },
        fresh: None,
    };
    *preserved.host.kept_claim.lock().unwrap() = Some(kept(FAILED_RUN));

    let (setup, resumed) = preserved.resumed();
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "continuation", "{resumed}");
    assert_eq!(resumed["source_machine_id"], "hm_local", "{resumed}");
    assert_eq!(resumed["source_sha"], preserved.candidate.as_str());
    let checkout = workspace(&setup);
    assert_eq!(head(&checkout), base);
    assert_eq!(status(&checkout), "?? feature.txt");
    assert_resume_recorded(&preserved, "resumed_repaired");
    let history = preserved.host.history(RESUME_TASK);
    let note = history.into_iter().rev().find_map(|entry| entry.note);
    let note = note.unwrap_or_default();
    assert!(note.contains("claim=claim-1, machine=hm_local"), "{note}");

    *preserved.host.kept_claim.lock().unwrap() = Some(kept("jrun-other-leaf"));
    let fresh = preserved.requeue("jrun-after-stale-claim").1;
    let fresh = fresh.expect("candidate_resume");
    assert_eq!(fresh["outcome"], "fresh", "{fresh}");
    assert_eq!(fresh["reason_code"], "no_candidate", "{fresh}");
}

/// [ORB-14261] A repair claim's leaf re-applies the candidate the owner's
/// stopped landing preserved onto its fresh base. The implementer always gets
/// it: a conflict to resolve against what the base gained, or a clean apply to
/// confirm on the moved base. Neither reads nor writes the owner's task.
fn a_repair_claim_resumes_its_stopped_landing_candidate_for_the_implementer() {
    let conflicting = PreservedCandidate::new("base.txt", "candidate\n");
    let base = conflicting.advance_base();
    let history = conflicting.host.history(RESUME_TASK).len();
    let setup = conflicting.next_setup();

    let resumed = conflicting.resume_claim_repair(&setup, &conflicting.candidate);
    let resumed = resumed.expect("candidate_resume");
    assert_resumed("conflicting", &resumed, "resumed_repaired", true);
    assert_eq!(resumed["repair"]["trigger"], "conflict");
    assert_eq!(resumed["repair"]["conflicting_paths"], json!(["base.txt"]));
    assert!(
        resumed["repair"]["output"]
            .as_str()
            .unwrap()
            .contains("conflicts with its base"),
        "the implementer sees why the landing stopped: {resumed}"
    );
    assert_eq!(resumed["source_sha"], conflicting.candidate.as_str());
    let checkout = workspace(&setup);
    assert_eq!(head(&checkout), base);
    assert_both_sides_of_the_conflict(&checkout);
    let recorded = conflicting.host.history(RESUME_TASK).len();
    let message = "a claimed leaf records nothing on a task it does not own";
    assert_eq!(recorded, history, "{message}");

    let clean = PreservedCandidate::new("feature.txt", "feature\n");
    clean.advance_base();
    let setup = clean.next_setup();
    let resumed = clean.resume_claim_repair(&setup, &clean.candidate);
    let resumed = resumed.expect("candidate_resume");
    assert_resumed("clean", &resumed, "resumed_repaired", true);
    assert_eq!(resumed["repair"]["trigger"], "landing");
    let message = "the candidate is applied as uncommitted work";
    assert_eq!(status(&workspace(&setup)), "?? feature.txt", "{message}");

    let present = PreservedCandidate::new("present.txt", "present\n");
    commit_file(&present.fixture.repo, "present.txt", "present\n");
    let setup = present.next_setup();
    let resumed = present.resume_claim_repair(&setup, &present.candidate);
    let resumed = resumed.expect("candidate already on the new base");
    assert_eq!(resumed["outcome"], "resumed_repaired", "{resumed}");
    assert_eq!(resumed["repair"]["trigger"], "landing");
    let output = resumed["repair"]["output"].as_str().unwrap();
    assert!(output.contains("already present on the current base"));
    let message = "already-present work leaves the base checkout clean";
    assert_eq!(status(&workspace(&setup)), "", "{message}");

    let missing = present.resume_claim_repair(&setup, &"f".repeat(40));
    let missing = missing.expect_err("a repair may not silently discard an unavailable candidate");
    assert!(
        missing.to_string().contains("repair candidate")
            && missing.to_string().contains("could not be restored"),
        "the repair failure retains its cause: {missing}"
    );

    let fresh = clean.resume_claimed(&setup, json!({"claim_repair": null}));
    let fresh = fresh.expect("a first attempt carries no repair");
    assert_eq!(fresh["outcome"], "fresh", "{fresh}");
    assert_eq!(fresh["repair"], Value::Null);
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
    branch: String,
}

impl PreservedCandidate {
    fn new(file: &str, contents: &str) -> Self {
        let fixture = Fixture::new();
        let host = LifecycleHost::new(&fixture.repo);
        let (_, checkout) = setup_checkout(&host, RESUME_TASK, FAILED_RUN);
        let candidate = commit_file(&checkout.path, file, contents);
        let input = json!({ "task_ids": [RESUME_TASK] });
        host.add_run(job_run(FAILED_RUN, JobRunState::Failed, input));
        let preserved = Self {
            fixture,
            host,
            candidate,
            branch: checkout.branch,
        };
        preserved.preserve_step("validate");
        preserved.host.set_status(RESUME_TASK, TaskStatus::Backlog);
        preserved
    }

    /// The `feature.txt` candidate carried onto an advanced base under the
    /// required `command` and committed by the next run, with that run's
    /// setup and resume.
    fn committed(command: &str) -> (Self, Value, Value) {
        let preserved = Self::new("feature.txt", "feature\n");
        preserved.advance_base();
        preserved.require(&[command]);
        let (setup, resumed) = preserved.resumed();
        preserved.commit(&setup);
        (preserved, setup, resumed)
    }

    /// Advance the base past the candidate's with a `base.txt` edit.
    fn advance_base(&self) -> String {
        commit_file(&self.fixture.repo, "base.txt", "v2\n")
    }

    /// Set `workflow.required_validation_commands`.
    fn require(&self, commands: &[&str]) {
        self.host.set_required_commands(commands);
    }

    /// Point the failed run's handoff at `failed_step_id` and requeue.
    fn resume_failed_step(
        &self,
        failed_step_id: &str,
        run_id: &str,
    ) -> (Value, Result<Value, OrbitError>) {
        self.preserve_step(failed_step_id);
        self.requeue(run_id)
    }

    /// Requeue as `run_id`: its setup, and the resume that follows it.
    fn requeue(&self, run_id: &str) -> (Value, Result<Value, OrbitError>) {
        let setup = self.relinked_setup(run_id);
        let resumed = self.resume_for(&setup, run_id);
        (setup, resumed)
    }

    /// `run_id`'s setup, linked to the failed run.
    ///
    /// The task is linked back to the failed run first: each setup stamps
    /// its own run id, and resume reads the link from before that stamp.
    fn relinked_setup(&self, run_id: &str) -> Value {
        self.host.link_run(RESUME_TASK, FAILED_RUN);
        self.next_setup_for(run_id)
    }

    fn preserve_step(&self, failed_step_id: &str) {
        self.preserve_decision(failed_step_id, "blocked_failure_pr");
    }

    /// Record the failed run's handoff at `failed_step_id` as `decision`.
    fn preserve_decision(&self, failed_step_id: &str, decision: &str) {
        let task = self.host.get_task(RESUME_TASK).unwrap();
        let handoff = json!({
            "phase": "failure_handoff",
            "decision": decision,
            "task_id": RESUME_TASK,
            "handoff_run_id": FAILED_RUN,
            "branch": self.branch,
            "head_sha": self.candidate,
            "task_spec_digest": task.spec_digest(),
        });
        self.host.preserve(FAILED_RUN, failed_step_id, handoff);
    }

    /// [ORB-14450] Make `FAILED_RUN` a run held on named external evidence
    /// for `head_sha`, whose receipt is the task's latest decision: no
    /// failure checkpoint, the task's hold artifact, and the receipt event.
    fn hold(&self, head_sha: &str) {
        let mut state = pipeline_state(FAILED_RUN);
        state.pipeline = json!({"worktree": {"head_ref": self.branch}});
        let mut states = self.host.run_states.lock().unwrap();
        states.insert(FAILED_RUN.to_string(), state);
        drop(states);
        let task = self.host.get_task(RESUME_TASK).unwrap();
        let hold = json!({
            "schema_version": 1,
            "attempt_id": "attempt-1",
            "lineage_key": "lineage-1",
            "run_id": FAILED_RUN,
            "candidate": {"commit": head_sha, "tree": "held-tree"},
            "task_meaning_digest": "meaning",
            "requirements": [{
                "kind": "native_os", "name": "macOS run",
                "command": "native macos", "artifact": "evidence/macos.json",
            }],
            "task_spec_digest": task.spec_digest(),
        });
        let artifact = json_artifact("review-evidence-hold.json", &hold);
        self.host.set_artifact(RESUME_TASK, artifact);
        self.host.clear_history(RESUME_TASK);
        let receipt = TaskHistoryEntry {
            note: Some(format!(
                "run={FAILED_RUN}; all named external checks arrived for the held \
                 candidate; queued for fresh review."
            )),
            from_status: Some(TaskStatus::InProgress),
            to_status: Some(TaskStatus::Backlog),
            ..history_entry(Utc::now(), "system", "review_evidence_received")
        };
        self.host.record_history(RESUME_TASK, receipt);
    }

    /// The requeued task's next run sets up its checkout, linked to the
    /// failed run.
    fn next_setup(&self) -> Value {
        self.next_setup_for(NEXT_RUN)
    }

    fn next_setup_for(&self, run_id: &str) -> Value {
        let input = setup_input(&[RESUME_TASK], run_id);
        let setup = action(&self.host, "worktree_setup", &input)
            .unwrap_or_else(|error| panic!("the next run's setup ({run_id}): {error}"));
        assert_eq!(setup["prior_job_run_id"], FAILED_RUN);
        setup
    }

    /// The next run's setup and its resume, which must not error.
    fn resumed(&self) -> (Value, Value) {
        let setup = self.next_setup();
        let resumed = self.resume(&setup).expect("candidate_resume");
        (setup, resumed)
    }

    /// A claimed leaf's resume with the claim's `fields`.
    fn resume_claimed(&self, setup: &Value, fields: Value) -> Result<Value, OrbitError> {
        let mut input = json!({
            "job_run_id": NEXT_RUN,
            "task_ids": [RESUME_TASK],
            "workspace_path": setup["workspace_path"],
            "base_sha": setup["base_sha"],
            "claimed": true,
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        action(&self.host, "candidate_resume", &input)
    }

    /// A claimed leaf's resume of the repair its claim carries for `head_sha`.
    fn resume_claim_repair(&self, setup: &Value, head_sha: &str) -> Result<Value, OrbitError> {
        let repair = json!({
            "repairs_claim_id": "claim-1",
            "handoff_id": "handoff-1",
            "branch": self.branch,
            "head_sha": head_sha,
            "stop_evidence": "pull request #42 conflicts with its base",
        });
        self.resume_claimed(setup, json!({"claim_repair": repair}))
    }

    fn commit(&self, setup: &Value) -> Value {
        action(
            &self.host,
            "git_commit",
            &json!({
                "job_run_id": NEXT_RUN, "scope": "all",
                "workspace_path": setup["workspace_path"],
                "base_ref": setup["base_ref"], "base_sha": setup["base_sha"],
            }),
        )
        .expect("commit carried candidate")
    }

    fn validate(&self, setup: &Value) -> Result<Value, OrbitError> {
        action(
            &self.host,
            "candidate_validate",
            &json!({
                "job_run_id": NEXT_RUN, "completed_task_ids": [RESUME_TASK],
                "workspace_path": setup["workspace_path"],
                "base_ref": setup["base_ref"], "base_sha": setup["base_sha"],
            }),
        )
    }

    fn validation_log(&self) -> Value {
        let artifacts = self.host.artifacts.lock().unwrap();
        let log = artifacts[RESUME_TASK]
            .iter()
            .find(|artifact| artifact.path == format!("validation/{NEXT_RUN}/0.json"))
            .expect("durable final validation evidence");
        serde_json::from_slice(&log.content).unwrap()
    }

    fn resume(&self, setup: &Value) -> Result<Value, OrbitError> {
        self.resume_for(setup, NEXT_RUN)
    }

    fn resume_for(&self, setup: &Value, run_id: &str) -> Result<Value, OrbitError> {
        action(
            &self.host,
            "candidate_resume",
            &json!({
                "job_run_id": run_id,
                "task_ids": [RESUME_TASK],
                "workspace_path": setup["workspace_path"],
                "base_sha": setup["base_sha"],
                "prior_job_run_id": setup["prior_job_run_id"],
            }),
        )
    }
}

/// `resumed` has `outcome` and runs the implementer exactly when `implement`.
fn assert_resumed(case: &str, resumed: &Value, outcome: &str, implement: bool) {
    assert_eq!(resumed["outcome"], outcome, "{case}: {resumed}");
    assert_eq!(resumed["implement"], implement, "{case}: {resumed}");
}

/// The candidate's `base.txt` conflict reaches the implementer with both
/// sides and their markers.
fn assert_both_sides_of_the_conflict(checkout: &Path) {
    let conflicted = read(checkout.join("base.txt"));
    assert!(
        conflicted.contains("<<<<<<<")
            && conflicted.contains("candidate")
            && conflicted.contains("v2"),
        "the implementer starts from both sides: {conflicted}"
    );
}

/// The task history names the outcome, the source run and the candidate SHA.
fn assert_resume_recorded(preserved: &PreservedCandidate, outcome: &str) {
    assert_resume_recorded_for(preserved, outcome, &preserved.candidate);
}

fn assert_resume_recorded_for(preserved: &PreservedCandidate, outcome: &str, source_sha: &str) {
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
        format!("source_sha={source_sha}"),
    ] {
        assert!(note.contains(&expected), "{expected} in {note}");
    }
}

fn discard_entry(at: chrono::DateTime<Utc>) -> TaskHistoryEntry {
    history_entry(at, "human:operator", CANDIDATE_DISCARDED_EVENT)
}

/// A history `event` by `by` at `at`, without a note or status change.
fn history_entry(at: chrono::DateTime<Utc>, by: &str, event: &str) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at,
        by: by.to_string(),
        event: event.to_string(),
        note: None,
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

fn json_artifact(path: &str, content: &Value) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content: content.to_string().into_bytes(),
        media_type: "application/json".to_string(),
        created_by: Some("system".to_string()),
    }
}

// ---------------------------------------------------------------------------
// pr_prepare + git_rebase
// ---------------------------------------------------------------------------

fn git_rebase_decision_follows_the_checkout_state() {
    // A candidate that does not overlap the advanced base is rebased.
    let clean = PreparedRebase::new("jrun-rebase-clean", "candidate.txt", "base.txt");
    let rebased = clean.rebase().expect("non-overlapping rebase");
    assert_eq!(rebased["decision"], "performed", "{rebased:#}");
    assert_ne!(clean.head(), clean.candidate);
    assert!(is_ancestor(&clean.checkout.path, &clean.target, "HEAD"));
    let edit = read(clean.checkout.path.join("candidate.txt"));
    assert_eq!(edit, "candidate\n");
    assert!(!rebase_in_progress(&clean.checkout.path));

    // Overlapping edits stop the rebase and hand it to conflict recovery
    // as a typed conflict, with the rebase left in place to resolve.
    let conflicted = PreparedRebase::new("jrun-rebase-conflict", "README.md", "README.md");
    let checkout = &conflicted.checkout.path;
    let error = conflicted.rebase().expect_err("overlapping rebase");
    let OrbitError::RecoverableVcsConflict(conflict) = &error else {
        panic!("expected a recoverable conflict, got {error}");
    };
    assert_eq!(conflict.operation, "git_rebase");
    assert_eq!(conflict.target_base_sha, conflicted.target);
    assert_eq!(conflict.original_base_sha, conflicted.base_sha);
    assert_eq!(conflict.conflicting_paths, ["README.md"]);
    assert_eq!(
        conflict.conflicting_files,
        conflicted_file("README.md", false)
    );
    // Supply real unmerged index entries whose modes and IDs prove a
    // trivial resolution. Re-entering git_rebase diagnoses the live stop
    // without changing the index or authorizing a merge.
    let (_, entries) = unmerged_entries(checkout);
    for (sources, deterministic) in [
        ([Some(0), Some(1), Some(1)], true),
        ([Some(0), Some(0), Some(2)], true),
        ([Some(0), Some(1), Some(0)], true),
        ([None, Some(1), Some(1)], true),
        ([None, Some(1), Some(2)], false),
    ] {
        use std::io::Write;
        // Clear the old stages so absent entries really are absent.
        let mut input = format!("0 {}\tREADME.md\n", "0".repeat(entries[0].1.len()));
        for (index, source) in sources.iter().enumerate() {
            let Some(source) = source else { continue };
            let (mode, object, _, _) = &entries[*source];
            input += &format!("{mode} {object} {}\tREADME.md\n", index + 1);
        }
        let mut command = crate::git_test_support::command()
            .current_dir(checkout)
            .args(["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = command.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
        drop(stdin);
        assert!(command.wait_with_output().unwrap().status.success());
        let (before, _) = unmerged_entries(checkout);
        let OrbitError::RecoverableVcsConflict(diagnosed) = conflicted.rebase().unwrap_err() else {
            panic!("expected the stopped rebase's structured conflict");
        };
        let files = conflicted_file("README.md", deterministic);
        let message = format!("stage sources: {sources:?}");
        assert_eq!(diagnosed.conflicting_files, files, "{message}");
        assert_eq!(unmerged_entries(checkout).0, before, "{message}");
        assert!(rebase_in_progress(checkout), "{message}");
    }

    assert!(rebase_in_progress(checkout));
    assert!(!git(checkout, &["ls-files", "-u"]).is_empty());

    // A rebase Orbit did not start is someone else's work: refused and
    // left exactly as found.
    let foreign = PreparedRebase::new("jrun-rebase-foreign", "candidate.txt", "base.txt");
    let rebase_dir = git_path(&foreign.checkout.path, "rebase-merge");
    fs::create_dir_all(&rebase_dir).unwrap();
    let orig_head = format!("{}\n", "a".repeat(40));
    let onto = format!("{}\n", foreign.target);
    for (file, contents) in [
        ("orig-head", orig_head.as_str()),
        ("onto", onto.as_str()),
        ("head-name", "refs/heads/foreign-branch\n"),
        ("git-rebase-todo", ""),
        ("end", "1\n"),
        ("msgnum", "1\n"),
    ] {
        fs::write(rebase_dir.join(file), contents).unwrap();
    }
    let error = foreign.rebase().expect_err("foreign rebase is refused");
    assert!(
        !matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "a foreign rebase is not a conflict to recover: {error}"
    );
    let message = "the refusal explains itself";
    assert!(
        error.to_string().contains("pre-existing rebase"),
        "{message}: {error}"
    );
    assert!(rebase_dir.join("orig-head").is_file());
    assert_eq!(foreign.head(), foreign.candidate);
}

fn git_rebase_rename_delete_conflicts_remain_for_recovery() {
    for candidate_renames in [true, false] {
        let edit = |repo: &Path, rename: bool| {
            if rename {
                git(repo, &["mv", "README.md", "renamed.md"]);
            } else {
                git(repo, &["rm", "README.md"]);
            }
            git(repo, &["commit", "-m", "rename or delete"]);
            head(repo)
        };
        let prepared = PreparedRebase::with_edits(
            "jrun-rebase-rename-delete",
            |repo| edit(repo, candidate_renames),
            |repo| edit(repo, !candidate_renames),
        );
        let checkout = &prepared.checkout.path;
        let OrbitError::RecoverableVcsConflict(conflict) = prepared.rebase().unwrap_err() else {
            panic!("expected Git's rename/delete conflict");
        };
        let case = format!("candidate renames: {candidate_renames}");
        let (index, entries) = unmerged_entries(checkout);
        let message = "Git produced a missing-side conflict";
        assert_eq!(entries.len(), 2, "{case}: {message}");
        assert_eq!(entries[0].2, "1", "{case}");
        let theirs = if candidate_renames { "3" } else { "2" };
        assert_eq!(entries[1].2, theirs, "{case}");
        assert_eq!(entries[0].0, entries[1].0, "{case}: unchanged mode");
        assert_eq!(entries[0].1, entries[1].1, "{case}: unchanged content");
        assert!(
            entries.iter().all(|entry| entry.3 == "renamed.md"),
            "{case}"
        );
        assert_eq!(conflict.conflicting_paths, ["renamed.md"], "{case}");
        let files = conflicted_file("renamed.md", false);
        assert_eq!(conflict.conflicting_files, files, "{case}");

        // Re-observing the stop supplies diagnostics without resolving
        // the rename/delete choice or certifying recovery.
        let head = prepared.head();
        let OrbitError::RecoverableVcsConflict(diagnosed) = prepared.rebase().unwrap_err() else {
            panic!("expected the stopped rebase's structured conflict");
        };
        assert_eq!(
            diagnosed.conflicting_files, conflict.conflicting_files,
            "{case}"
        );
        assert_eq!(unmerged_entries(checkout).0, index, "{case}");
        assert_eq!(prepared.head(), head, "{case}");
        assert!(rebase_in_progress(checkout), "{case}");
        assert!(prepared.host.checkpoints().is_empty(), "{case}");
    }
}

/// The checkout's unmerged index listing, and its entries as
/// `(mode, object, stage, path)`.
fn unmerged_entries(checkout: &Path) -> (String, Vec<(String, String, String, String)>) {
    let listing = git(checkout, &["ls-files", "--unmerged"]);
    let entries = listing
        .lines()
        .map(|line| {
            let (entry, path) = line.split_once('\t').unwrap();
            let fields = entry.split_whitespace().collect::<Vec<_>>();
            let [mode, object, stage] = fields[..] else {
                panic!("unmerged entry {line}");
            };
            let fields = (mode.to_string(), object.to_string(), stage.to_string());
            (fields.0, fields.1, fields.2, path.to_string())
        })
        .collect();
    (listing, entries)
}

fn conflicted_file(
    path: &str,
    deterministically_resolvable: bool,
) -> Vec<orbit_common::ConflictedFile> {
    vec![orbit_common::ConflictedFile {
        path: path.to_string(),
        deterministically_resolvable,
    }]
}

// ---------------------------------------------------------------------------
// pr_conflict_recovery
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn conflict_recovery_leaf_completes_only_its_checkpointed_rebase() {
    let prepared = PreparedRebase::new("jrun-recovery", "README.md", "README.md");
    let target = prepared.target.clone();
    let conflict = stopped_conflict(&prepared);
    let launched = prepared.fixture.root.path().join("provider-launched");
    let body = format!(
        ": > '{}'\nprintf 'candidate and target\\n' > README.md",
        launched.display()
    );
    let host = prepared.with_provider(&body);
    let recovery_input = conflict_recovery_input(&prepared, &conflict);

    // A checkpoint that does not describe the stopped rebase is
    // refused before the provider runs or Git is touched.
    let mut mismatched = recovery_input.clone();
    mismatched["target_base_sha"] = json!(prepared.candidate);
    let error = recover(&host, &prepared.run_id, mismatched);
    let error = error.expect_err("mismatched checkpoint is refused");
    let refusal = error.to_string();
    assert!(refusal.contains("existing rebase matching"), "{error}");
    assert!(!launched.exists(), "the provider must not launch");
    assert!(rebase_in_progress(&prepared.checkout.path));
    assert!(host.checkpoints().is_empty());

    let outcome = recover(&host, &prepared.run_id, recovery_input);
    let outcome = outcome.expect("checkpointed rebase is recovered");
    assert!(outcome.success, "{:?}", outcome.message);
    assert!(launched.exists());
    assert!(!rebase_in_progress(&prepared.checkout.path));
    let resolved = read(prepared.checkout.path.join("README.md"));
    assert_eq!(resolved, "candidate and target\n");
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
}

/// [F2026-10-041] One run recovers the same step twice. Recovery A lands the
/// candidate on the pinned base and keeps that result when the advanced base
/// conflicts again, so the retry refuses the moved base. The run resumes from
/// preparation, re-pins to the advanced base, conflicts again, and recovery B
/// completes as a new host-reserved attempt instead of colliding with A. The
/// retry then delivers B's exact evidence and nothing older.
#[cfg(unix)]
fn a_resumed_run_recovers_the_same_step_again_as_a_new_attempt() {
    let prepared = PreparedRebase::new("jrun-reattempt", "README.md", "README.md");
    let host = prepared.with_provider("printf 'resolved\\n' > README.md");
    let checkout = &prepared.checkout.path;

    // The base advances with another README edit while A is pending.
    let conflict = stopped_conflict(&prepared);
    let advanced = commit_file(&prepared.fixture.repo, "README.md", "advanced\n");
    let input = recovery_input_for(&prepared, &prepared.prepared, &conflict);
    let outcome = recover(&host, &prepared.run_id, input).expect("recovery A completes");
    assert!(outcome.success, "{:?}", outcome.message);
    let head_a = prepared.head();
    let [(_, _, recovery_a)] = host.checkpoints().try_into().unwrap();
    assert_eq!(recovery_a["recovery_attempt"], 1);
    let message = "A kept the pinned result";
    assert_eq!(recovery_a["base_sha"], prepared.target, "{message}");

    // The pinned-base freshness refusal stands, and touches nothing.
    let error = prepared.rebase_on(&host, &prepared.prepared);
    let error = error.expect_err("the retry refuses the moved base");
    assert!(
        error.to_string().contains("moved from checkpoint"),
        "{error}"
    );
    assert_eq!(prepared.head(), head_a);
    assert_eq!(status(checkout), "");

    // Same-run resume: prepare again, now against the advanced base.
    let resumed = action(&host, "pr_prepare", &prepared.common).expect("re-prepare");
    assert_eq!(resumed["head_sha"], head_a);
    assert_eq!(resumed["base_sha"], advanced);
    let again = prepared.rebase_on(&host, &resumed);
    let conflict = conflict_fields(again.expect_err("the advanced base conflicts again"));
    let input = recovery_input_for(&prepared, &resumed, &conflict);
    let outcome = recover(&host, &prepared.run_id, input);
    let outcome = outcome.expect("recovery B completes as a new attempt");
    assert!(outcome.success, "{:?}", outcome.message);
    let head_b = prepared.head();
    assert_ne!(head_b, head_a);

    let checkpoints = host.checkpoints();
    let [(_, step_a, kept_a), (_, step_b, recovery_b)] = checkpoints.as_slice() else {
        panic!("expected two recovery checkpoints, got {checkpoints:#?}");
    };
    let steps = (step_a.as_str(), step_b.as_str());
    assert_eq!(steps, ("sync_base", "sync_base"));
    assert_eq!(kept_a, &recovery_a, "A's evidence is not rewritten");
    assert_eq!(recovery_b["recovery_attempt"], 2);
    assert_eq!(recovery_b["head_sha_before"], head_a);
    assert_eq!(recovery_b["base_sha"], advanced);
    assert_eq!(recovery_b["head_sha"], head_b);
    let scopes = host
        .recovery_attempts()
        .into_iter()
        .map(|(_, _, scope)| (scope.head_sha_before, scope.target_base_sha))
        .collect::<Vec<_>>();
    assert_eq!(
        scopes,
        vec![
            (prepared.candidate.clone(), prepared.target.clone()),
            (head_a.clone(), advanced.clone()),
        ],
        "each recovery reserved its own attempt for its own stopped rebase"
    );

    // A leaf replaying A's evidence cannot vouch for B's HEAD.
    host.leaf_writes_recovery(&prepared.run_id, "sync_base", recovery_a.clone());
    let error = prepared.rebase_on(&host, &resumed);
    let error = error.expect_err("stale evidence does not describe the recovered HEAD");
    let refusal = "no exact host-validated recovery checkpoint";
    assert!(error.to_string().contains(refusal), "{error}");
    assert_eq!(prepared.head(), head_b);
    assert_eq!(status(checkout), "");

    // B's exact evidence delivers B.
    host.leaf_writes_recovery(&prepared.run_id, "sync_base", recovery_b.clone());
    let retried = prepared.rebase_on(&host, &resumed);
    let retried = retried.expect("the retry reuses recovery B");
    assert_eq!(retried["decision"], "reused_recovery");
    assert_eq!(retried["head_sha"], head_b);
    assert_eq!(retried["base_sha"], advanced);
}

/// The recovery input for the stopped rebase of the handoff `preparation`
/// describes, carrying the failed step's prepared fields.
fn recovery_input_for(prepared: &PreparedRebase, preparation: &Value, conflict: &Value) -> Value {
    let mut input = conflict_recovery_input(prepared, conflict);
    input["failed_step_input"]["head_sha"] = preparation["head_sha"].clone();
    input["failed_step_input"]["remote_sha"] = preparation["remote_sha"].clone();
    input
}

/// A provider shell that runs `body` in the checkout and reports success.
fn provider_script(body: &str) -> String {
    format!(
        "#!/bin/sh\nset -eu\ncat > /dev/null\n{body}\nprintf '%s\\n' '{{\"schemaVersion\":1,\"status\":\"success\",\"result\":{{}},\"error\":null}}'\n"
    )
}

/// Install a substitute `codex` under `root` that runs `body` in the
/// checkout and reports success.
#[cfg(unix)]
fn install_provider(root: &Path, body: &str) -> PathBuf {
    let provider = root.join("codex");
    write_executable(&provider, &provider_script(body));
    provider
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
    conflict_fields(prepared.rebase().expect_err("overlapping rebase"))
}

/// A stopped rebase's conflict, as the recovery input's conflict fields.
fn conflict_fields(error: OrbitError) -> Value {
    let OrbitError::RecoverableVcsConflict(conflict) = error else {
        panic!("expected a recoverable conflict, got {error}");
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
fn conflict_recovery_commits_companion_edits_with_the_resolution() {
    let prepared = PreparedRebase::new("jrun-companion", "README.md", "README.md");
    let target = prepared.target.clone();
    let checkout = &prepared.checkout.path;
    let conflict = stopped_conflict(&prepared);
    let host = prepared.with_provider(
        "printf 'pub use moved::export;\\n' > README.md\nmkdir -p src/moved .orbit/tmp\nmv base.txt src/moved/mod.rs\nprintf 'scratch\\n' > .orbit/tmp/notes.md",
    );

    let outcome = prepared.recover_conflict(&host, &conflict);
    let outcome = outcome.expect("a resolution with companion edits continues the rebase");
    assert!(outcome.success, "{:?}", outcome.message);
    assert!(!rebase_in_progress(checkout));
    assert!(is_ancestor(checkout, &target, "HEAD"));
    let show = ["show", "--format=", "--name-only", "--no-renames", "HEAD"];
    assert_eq!(
        git(checkout, &show),
        "README.md\nbase.txt\nsrc/moved/mod.rs",
        "the continued commit holds the resolution and both sides of the move"
    );
    assert_eq!(status(checkout), "");
    let scratch = read(checkout.join(".orbit/tmp/notes.md"));
    let message = "scratch stays in place and out of the commit";
    assert_eq!(scratch, "scratch\n", "{message}");
    let checkpoints = host.checkpoints();
    let [(_, _, checkpoint)] = checkpoints.as_slice() else {
        panic!("expected one recovery checkpoint, got {checkpoints:#?}");
    };
    let companions = json!(["base.txt", "src/moved/mod.rs"]);
    assert_eq!(checkpoint["companion_paths"], companions);
    assert_eq!(
        host.widenings(),
        vec![(
            "T-REBASE".to_string(),
            ContextWideningStep::Recovery,
            "pr_conflict_recovery".to_string(),
            vec!["base.txt".to_string(), "src/moved/mod.rs".to_string()],
        )]
    );
}

/// [ORB-14332] Both commits of a two-commit candidate conflict with the
/// advanced base. The first recovery resolves the first stop; continuing the
/// rebase stops again on the second commit, which the host keeps instead of
/// refusing: the resolved pick stays, nothing is certified, and the retried
/// `git_rebase` reports the new stop pinned to the same base. A second
/// recovery round resolves it and certifies the whole rewrite, which the
/// retry then delivers.
#[cfg(unix)]
fn conflict_recovery_resolves_each_stop_of_a_multi_commit_rebase() {
    let prepared = PreparedRebase::with_commits(
        "jrun-multi-stop",
        &[("README.md", "candidate\n"), ("base.txt", "candidate v2\n")],
        &[("README.md", "target\n"), ("base.txt", "target v2\n")],
    );
    let target = prepared.target.clone();
    let checkout = &prepared.checkout.path;

    let first = stopped_conflict(&prepared);
    assert_eq!(first["conflicting_paths"], json!(["README.md"]));
    let host = prepared.with_provider("printf 'candidate and target\\n' > README.md");
    let outcome = prepared.recover_conflict(&host, &first);
    let outcome = outcome.expect("the first stop is recovered although the rebase stops again");
    assert!(outcome.success, "{:?}", outcome.message);
    assert!(rebase_in_progress(checkout), "stopped on the second commit");
    let message = "an unfinished rebase is not certified";
    assert!(host.checkpoints().is_empty(), "{message}");
    let first_pick = git(checkout, &["show", "HEAD:README.md"]);
    let message = "the first resolution is kept";
    assert_eq!(first_pick, "candidate and target", "{message}");

    let second = prepared.rebase_on(&host, &prepared.prepared);
    let second = conflict_fields(second.expect_err("the retry reports the new stop"));
    assert_eq!(second["conflicting_paths"], json!(["base.txt"]));
    assert_eq!(second["target_base_sha"], target);
    assert!(rebase_in_progress(checkout), "the retry touches nothing");

    let second_resolution = "printf 'candidate and target v2\\n' > base.txt";
    install_provider(prepared.fixture.root.path(), second_resolution);
    let outcome = prepared.recover_conflict(&host, &second);
    let outcome = outcome.expect("the second stop completes the rebase");
    assert!(outcome.success, "{:?}", outcome.message);
    assert!(!rebase_in_progress(checkout));
    let head = prepared.head();
    let count = ["rev-list", "--count", &format!("{target}..HEAD")];
    let message = "both candidate commits sit on the pinned base";
    assert_eq!(git(checkout, &count), "2", "{message}");
    assert_eq!(read(checkout.join("README.md")), "candidate and target\n");
    assert_eq!(read(checkout.join("base.txt")), "candidate and target v2\n");
    let checkpoints = host.checkpoints();
    let [(_, step_id, checkpoint)] = checkpoints.as_slice() else {
        panic!("expected one recovery checkpoint, got {checkpoints:#?}");
    };
    assert_eq!(step_id, "sync_base");
    assert_eq!(checkpoint["head_sha"], head);
    assert_eq!(checkpoint["head_sha_before"], prepared.candidate);
    assert_eq!(checkpoint["base_sha"], target);
    let message = "each round reserves its own attempt";
    assert_eq!(checkpoint["recovery_attempt"], 2, "{message}");

    let retried = prepared.rebase_on(&host, &prepared.prepared);
    let retried = retried.expect("the retry delivers the rewrite");
    assert_eq!(retried["decision"], "reused_recovery");
    assert_eq!(retried["head_sha"], head);
    assert_eq!(retried["base_sha"], target);
}

/// [ORB-13990] Companion edits never excuse the conflict itself: a conflict
/// path left unrepaired, or repaired with markers still in it, refuses the
/// continuation with the rebase left stopped and nothing widened. Staging
/// stays host-owned: a provider that stages its own companion is refused.
#[cfg(unix)]
fn conflict_recovery_still_refuses_unresolved_conflicts_beside_companion_edits() {
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
        let body = format!("{resolution}printf 'companion\\n' > companion.txt");
        let host = prepared.with_provider(&body);

        let error = prepared.recover_conflict(&host, &conflict);
        let error = error.expect_err("an unresolved conflict path refuses continuation");
        assert!(error.to_string().contains(refusal), "{run_id}: {error}");
        assert!(rebase_in_progress(&prepared.checkout.path), "{run_id}");
        assert!(host.checkpoints().is_empty(), "{run_id}");
        assert!(host.widenings().is_empty(), "{run_id}");
        let companion = read(prepared.checkout.path.join("companion.txt"));
        let message = "the provider's bytes stay for diagnosis";
        assert_eq!(companion, "companion\n", "{run_id}: {message}");
    }
}

/// [ORB-13990] Implementer and recovery agents may change any path the work
/// requires. As each exits, the boundary records the paths it changed on its
/// task, with the step that introduced them; host-owned `.orbit/` state and
/// gitignored output are never attributed. The reviewer's changes widen at
/// review settlement instead.
#[cfg(unix)]
fn agent_changed_paths_widen_selectors_with_their_step_as_the_agent_exits() {
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
        let (_, checkout) = setup_checkout(&host, "T-WIDEN", &run_id);
        let provider = install_provider(
            fixture.root.path(),
            "printf 'edited\\n' > README.md\nmkdir -p tests .orbit/tmp target\nprintf 'new\\n' > tests/new_case.rs\nprintf 'scratch\\n' > .orbit/tmp/log\nprintf 'built\\n' > target/out",
        );
        let host = host.with_provider(&provider);

        let outcome = dispatch_linked_activity(&host, activity, &run_id, "T-WIDEN", &checkout.path)
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
}

/// A provider that writes the primary checkout fails the boundary, and both
/// trees keep the bytes the provider left. Disjoint source dirt is enough:
/// overlap with the candidate is not what makes the edit fatal.
#[cfg(unix)]
fn provider_primary_source_edit_fails_closed_and_preserves_both_checkouts() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-ESCAPE", "jrun-primary-escape");
    let primary_head = head(&fixture.repo);
    let assigned_head = head(&checkout.path);

    let primary = fixture.repo.display().to_string();
    let provider = install_provider(
        fixture.root.path(),
        &format!(
            "printf 'assigned stays\\n' > assigned-stays.txt\nprintf 'primary readme\\n' > '{primary}/README.md'\nprintf 'escaped\\n' > '{primary}/escaped.txt'\ngit -C '{primary}' add -- README.md escaped.txt",
        ),
    );
    let host = host.with_provider(&provider);
    let error = dispatch_linked_provider(&host, "jrun-primary-escape", "T-ESCAPE", &checkout.path)
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
    assert_eq!(head(&fixture.repo), primary_head);
    assert_eq!(head(&checkout.path), assigned_head);
    assert_eq!(read(fixture.repo.join("README.md")), "primary readme\n");
    assert_eq!(read(fixture.repo.join("escaped.txt")), "escaped\n");
    assert_eq!(
        staged_paths(&fixture.repo),
        BTreeSet::from(["README.md".to_string(), "escaped.txt".to_string()]),
        "the guard must not reset the provider-mutated primary index"
    );
    assert_eq!(
        read(checkout.path.join("assigned-stays.txt")),
        "assigned stays\n"
    );
    assert!(
        !checkout.path.join("escaped.txt").exists(),
        "the guard must not copy the primary edit into the assigned checkout"
    );
    assert_eq!(
        read(checkout.path.join("README.md")),
        "base\n",
        "the assigned candidate keeps the bytes it had"
    );
}

/// User Git presentation settings must not hide a second edit to tracked WIP
/// or change the fingerprints and patches needed to detect and recover it.
#[cfg(unix)]
fn user_git_diff_config_preserves_dirty_fingerprints_and_recovery() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-DIFF-CONFIG", "jrun-diff-config-setup");
    let restore = fixture.root.path().join("restore");
    git(
        &fixture.repo,
        &["worktree", "add", "--detach", path_str(&restore), "HEAD"],
    );
    let provider = install_provider(
        fixture.root.path(),
        &format!(
            "printf 'candidate output\\n' > README.md\nprintf 'provider primary output\\n' > '{}/README.md'",
            fixture.repo.display(),
        ),
    );
    let host = host.with_provider(&provider);
    let global_config = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".gitconfig");
    let cases: &[&[(&str, &str)]] = &[
        &[],
        &[("diff.noprefix", "true")],
        &[("diff.mnemonicPrefix", "true")],
        &[("color.ui", "always")],
        &[
            ("diff.noprefix", "true"),
            ("diff.mnemonicPrefix", "true"),
            ("color.ui", "always"),
        ],
    ];
    let mut baseline = None;
    for (index, settings) in cases.iter().enumerate() {
        // This HOME belongs only to the isolated child. Each case
        // starts without any settings from the preceding case.
        fs::write(&global_config, super::git_fixture::ISOLATED_GIT_CONFIG).unwrap();
        for (key, value) in *settings {
            git(&fixture.repo, &["config", "--global", key, value]);
            assert_eq!(
                git(&fixture.repo, &["config", "--global", "--get", key]),
                *value
            );
        }
        for (root, staged, unstaged) in [
            (&fixture.repo, "operator staged\n", "operator unstaged\n"),
            (&checkout.path, "candidate staged\n", "candidate unstaged\n"),
        ] {
            fs::write(root.join("README.md"), staged).unwrap();
            git(root, &["add", "README.md"]);
            fs::write(root.join("README.md"), unstaged).unwrap();
        }
        let run_id = format!("jrun-diff-config-{index}");
        let blobs = TempDir::new().unwrap();
        let (sink, audit) = audit_writer(&run_id, blobs.path());
        let error = dispatch_audited_linked_activity(
            &host,
            "agent_implement",
            &run_id,
            "T-DIFF-CONFIG",
            &checkout.path,
            audit,
        )
        .expect_err(&format!(
            "{settings:?}: a second primary WIP edit must fail the boundary under user Git config",
        ));
        let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
        assert_eq!(diagnostic["primary_changed_paths"], json!(["README.md"]));
        let fingerprints = fingerprints(&sink, &diagnostic);
        let before = &fingerprints["primary_before"];
        let after = &fingerprints["primary_after"];
        assert_ne!(
            before["tracked_patch_sha256"], after["tracked_patch_sha256"],
            "{settings:?}: second edit changes the fingerprint"
        );
        let before_path = &before["path_states"]["README.md"];
        let after_path = &after["path_states"]["README.md"];
        assert!(before_path["staged_patch_sha256"].is_string());
        assert!(before_path["worktree_patch_sha256"].is_string());
        assert_ne!(
            before_path["worktree_patch_sha256"],
            after_path["worktree_patch_sha256"]
        );

        let patch_path = Path::new(diagnostic["recovery"]["tracked_patch"].as_str().unwrap());
        let patch = fs::read(patch_path).expect("durable recovery patch");
        if let Some((expected_fingerprints, expected_patch)) = &baseline {
            assert_eq!(
                &fingerprints, expected_fingerprints,
                "{settings:?}: full fingerprints match clean Git config"
            );
            assert_eq!(
                &patch, expected_patch,
                "{settings:?}: recovery bytes match clean Git config"
            );
        } else {
            baseline = Some((fingerprints, patch));
        }
        git(&restore, &["apply", "--binary", path_str(patch_path)]);
        assert_eq!(read(restore.join("README.md")), "candidate output\n");
        fs::write(restore.join("README.md"), "base\n").unwrap();
    }
}

/// Record-store dirt on a path the run also changed is still drift. The
/// `.orbit/` prefix alone must not excuse an intersection with the candidate.
#[cfg(unix)]
fn stationary_record_store_dirt_overlapping_the_run_fails_closed() {
    let fixture = Fixture::new();
    let record = ".orbit/auto_tasks/nightly.yaml";
    commit_tracked_record(&fixture.repo, record, "name: nightly\n");
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-OVERLAP", "jrun-record-overlap");
    let primary_head = head(&fixture.repo);

    let primary = fixture.repo.display().to_string();
    let provider = install_provider(
        fixture.root.path(),
        &format!(
            "mkdir -p .orbit/auto_tasks\nprintf 'name: from-run\\n' > '{record}'\nprintf 'name: from-primary\\n' > '{primary}/{record}'",
        ),
    );
    let host = host.with_provider(&provider);
    let error = dispatch_linked_provider(&host, "jrun-record-overlap", "T-OVERLAP", &checkout.path)
        .expect_err("record-store dirt on a run path is a boundary failure");

    let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
    assert_eq!(diagnostic["conflicting_paths"], json!([record]));
    assert_eq!(head(&fixture.repo), primary_head);
    assert_eq!(read(fixture.repo.join(record)), "name: from-primary\n");
    assert_eq!(read(checkout.path.join(record)), "name: from-run\n");
}

/// Concurrent record-store dirt that does not touch the run stays on the
/// primary and does not fail the provider. The provider itself only edits
/// the assigned checkout.
#[cfg(unix)]
fn stationary_record_store_dirt_disjoint_from_the_run_stays_benign() {
    let fixture = Fixture::new();
    let record = ".orbit/auto_tasks/nightly.yaml";
    commit_tracked_record(&fixture.repo, record, "name: nightly\n");
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-RECORD", "jrun-record-dirt");
    let primary_head = head(&fixture.repo);
    let ready = fixture.root.path().join("record-ready");
    let go = fixture.root.path().join("record-go");
    orbit_common::test_env::create_fixture_fifo(&go).unwrap();
    let primary_record = fixture.repo.join(record);

    let provider = install_provider(
        fixture.root.path(),
        &format!(
            ": > '{}'\nread -r _ < '{}'\nprintf 'candidate\\n' > candidate.txt",
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
        orbit_common::test_env::release_fixture_fifo(&go, Instant::now() + Duration::from_secs(20))
            .unwrap();
    });

    let outcome = dispatch_linked_provider(&host, "jrun-record-dirt", "T-RECORD", &checkout.path)
        .expect("disjoint record-store dirt is not primary drift");
    curator.join().expect("curator thread");
    assert!(outcome.success, "{:?}", outcome.message);
    assert_eq!(head(&fixture.repo), primary_head);
    assert_eq!(
        read(fixture.repo.join(record)),
        "name: nightly\nenabled: true\n",
        "the guard must leave concurrent record-store dirt in place"
    );
    assert_eq!(read(fixture.repo.join("README.md")), "base\n");
    assert_eq!(read(checkout.path.join("candidate.txt")), "candidate\n");
}

/// [ORB-14085] An operator edit to the primary between two dispatches of one
/// run is not provider drift: the second dispatch must capture a fresh "before"
/// instead of replaying the clean snapshot of the first. Unstaged edits and new
/// untracked files change neither HEAD nor the index mtime, so no cache key can
/// notice them.
#[cfg(unix)]
fn primary_dirtied_between_dispatches_of_one_run_is_not_provider_drift() {
    let fixture = Fixture::new();
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-REDISPATCH", "jrun-redispatch");

    let provider = install_provider(fixture.root.path(), "printf 'candidate\\n' > candidate.txt");
    let host = host.with_provider(&provider);

    let first = dispatch_linked_provider(&host, "jrun-redispatch", "T-REDISPATCH", &checkout.path)
        .expect("first dispatch");
    assert!(first.success, "{:?}", first.message);

    fs::write(fixture.repo.join("README.md"), "operator edit\n").unwrap();
    fs::write(fixture.repo.join("operator-notes.txt"), "scratch\n").unwrap();

    let second = dispatch_linked_provider(&host, "jrun-redispatch", "T-REDISPATCH", &checkout.path)
        .expect("a primary dirtied between dispatches is not provider drift");
    assert!(second.success, "{:?}", second.message);
    assert_eq!(read(fixture.repo.join("README.md")), "operator edit\n");
}

/// [ORB-14084] `git status --untracked-files=all` lists a nested repository as
/// one directory, and `git hash-object` cannot hash it. Both checkouts must
/// still fingerprint — committed repos by HEAD, an unborn repo as
/// `opaque-directory` — so dispatch proceeds, an agent-created clone is an
/// ordinary attributed edit, and a real boundary failure still writes recovery
/// that includes the directory.
#[cfg(unix)]
fn untracked_nested_repository_fingerprints_in_primary_and_assigned_worktree() {
    fn commit_nested(parent: &Path, relative: &str, body: &str) -> String {
        let dir = parent.join(relative);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init"]);
        git(&dir, &["config", "user.name", "Orbit Test"]);
        git(
            &dir,
            &["config", "user.email", "orbit-test@example.invalid"],
        );
        fs::write(dir.join("lib.txt"), body).unwrap();
        git(&dir, &["add", "lib.txt"]);
        git(&dir, &["commit", "-m", "nested"]);
        head(&dir)
    }

    fn seed_unborn(parent: &Path, relative: &str) {
        let dir = parent.join(relative);
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init"]);
    }

    fn identity_at<'a>(fingerprint: &'a Value, relative: &str) -> (&'a str, &'a str) {
        let entries = fingerprint["untracked_content"]
            .as_object()
            .unwrap_or_else(|| panic!("untracked_content missing: {fingerprint}"));
        let mut matches = entries
            .iter()
            .filter(|(path, _)| path.trim_end_matches('/') == relative);
        let Some((path, value)) = matches.next() else {
            panic!(
                "{relative} missing from {}",
                fingerprint["untracked_content"]
            );
        };
        assert!(
            matches.next().is_none(),
            "{relative} matched more than one untracked path"
        );
        (
            path.as_str(),
            value
                .as_str()
                .unwrap_or_else(|| panic!("{path} identity is not a string")),
        )
    }

    let fixture = Fixture::new();
    fs::write(fixture.repo.join("notes.txt"), "note\n").unwrap();
    let primary_head = commit_nested(&fixture.repo, "vendor/somelib", "lib\n");
    seed_unborn(&fixture.repo, "vendor/unborn");
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-NESTED", "jrun-nested-setup");
    let assigned_head = commit_nested(&checkout.path, "vendor/otherlib", "other\n");

    let provider = install_provider(fixture.root.path(), "");
    let host = host.with_provider(&provider);
    let outcome = dispatch_linked_provider(&host, "jrun-nested-stable", "T-NESTED", &checkout.path)
        .expect("a nested repository must not fail the checkout snapshot");
    assert!(
        outcome.success,
        "dispatch proceeds with nested repos in both checkouts: {:?}",
        outcome.message
    );
    assert!(
        host.widenings().is_empty(),
        "a nested repo already in the checkout is not a new agent edit: {:?}",
        host.widenings()
    );

    write_executable(
        &provider,
        &provider_script(
            "git init -q vendor/fresh\n\
             git -C vendor/fresh config user.name 'Orbit Test'\n\
             git -C vendor/fresh config user.email orbit-test@example.invalid\n\
             printf 'fresh\\n' > vendor/fresh/lib.txt\n\
             git -C vendor/fresh add lib.txt\n\
             git -C vendor/fresh commit -qm fresh\n",
        ),
    );
    let outcome =
        dispatch_linked_provider(&host, "jrun-nested-created", "T-NESTED", &checkout.path)
            .expect("an assigned clone must not fail verify");
    assert!(
        outcome.success,
        "an agent-created nested repo dispatches: {:?}",
        outcome.message
    );
    let widenings = host.widenings();
    assert_eq!(widenings.len(), 1, "{widenings:?}");
    let (task_id, step, activity, paths) = &widenings[0];
    assert_eq!(task_id, "T-NESTED");
    assert_eq!(*step, ContextWideningStep::Implement);
    assert_eq!(activity, "agent_implement");
    assert_eq!(
        paths
            .iter()
            .map(|path| path.trim_end_matches('/'))
            .collect::<Vec<_>>(),
        vec!["vendor/fresh"],
        "the new nested repository is the attributed path"
    );
    let fresh_head = git(&checkout.path.join("vendor/fresh"), &["rev-parse", "HEAD"]);

    let primary = fixture.repo.display().to_string();
    write_executable(
        &provider,
        &provider_script(&format!(
            "printf 'primary drift\\n' > '{primary}/README.md'\n"
        )),
    );
    let blobs = TempDir::new().unwrap();
    let run_id = "jrun-nested-drift";
    let (sink, audit) = audit_writer(run_id, blobs.path());
    let error = dispatch_audited_linked_activity(
        &host,
        "agent_implement",
        run_id,
        "T-NESTED",
        &checkout.path,
        audit,
    )
    .expect_err("primary drift beside a nested repo is still a boundary failure");
    let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
    assert!(
        error.is_non_retryable(),
        "nested-repo drift stays a non-retryable integrity failure"
    );
    assert!(
        diagnostic["recovery"].get("preservation_error").is_none(),
        "recovery must copy the nested directory: {diagnostic}"
    );
    assert_eq!(diagnostic["primary_dirt_paths"], json!(["README.md"]));

    let fingerprints = fingerprints(&sink, &diagnostic);
    for side in ["primary_before", "primary_after"] {
        let fingerprint = &fingerprints[side];
        let (path, identity) = identity_at(fingerprint, "vendor/somelib");
        assert_eq!(identity, format!("git-head:{primary_head}"), "{side}");
        assert_eq!(
            fingerprint["path_states"][path]["untracked_content_sha256"], identity,
            "{side}"
        );
        let (path, identity) = identity_at(fingerprint, "vendor/unborn");
        assert_eq!(identity, "opaque-directory", "{side}");
        assert_eq!(
            fingerprint["path_states"][path]["untracked_content_sha256"], identity,
            "{side}"
        );
        let notes = fingerprint["untracked_content"]["notes.txt"]
            .as_str()
            .expect("ordinary untracked file");
        assert!(
            notes.starts_with("git-blob:"),
            "{side}: file hashing still produces a blob identity, got {notes}"
        );
    }
    assert_eq!(
        fingerprints["primary_before"]["untracked_content"]["notes.txt"],
        fingerprints["primary_after"]["untracked_content"]["notes.txt"]
    );
    for side in ["assigned_before", "assigned_after"] {
        let fingerprint = &fingerprints[side];
        let (_, identity) = identity_at(fingerprint, "vendor/otherlib");
        assert_eq!(identity, format!("git-head:{assigned_head}"), "{side}");
        let (_, identity) = identity_at(fingerprint, "vendor/fresh");
        assert_eq!(identity, format!("git-head:{fresh_head}"), "{side}");
    }

    let payload = PathBuf::from(
        diagnostic["recovery"]["untracked_payload"]
            .as_str()
            .expect("recovery names the untracked payload"),
    );
    assert_eq!(
        read(payload.join("vendor/otherlib/lib.txt")),
        "other\n",
        "recovery copies a nested repository as a tree"
    );
    assert_eq!(read(payload.join("vendor/fresh/lib.txt")), "fresh\n");
    assert!(
        payload.join("vendor/otherlib/.git").is_dir(),
        "the nested git dir is part of the preserved tree"
    );
}

/// Every dirty integrity failure in one run preserves its own current
/// content, and the recovery store stays bounded: old attempts are pruned,
/// the newest never is.
#[cfg(unix)]
fn repeated_dirty_integrity_failures_keep_current_content_and_prune_old_attempts() {
    const FAILURES: usize = 8;
    let trace = TempDir::new().unwrap();
    let events = trace.path().join("git-events.jsonl");
    git(
        trace.path(),
        &[
            "config",
            "--global",
            "trace2.eventTarget",
            path_str(&events),
        ],
    );
    let earlier = Fixture::new();
    fs::write(earlier.repo.join("README.md"), "earlier row\n").unwrap();
    git(&earlier.repo, &["config", "user.name", "Earlier Row"]);
    let fixture = Fixture::new();
    let events: Vec<Value> = read(&events)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events.iter().any(|event| {
            event["event"] == "child_start"
                && event["argv"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|arg| arg == "maintenance"))
        }),
        "seed preparation must still execute native automatic Git maintenance"
    );
    assert!(
        !events.iter().any(|event| event["event"] == "region_enter"
            && event["category"] == "maintenance"
            && event["label"] == "detach"),
        "pristine seed maintenance must finish before copying: a detached daemon can remove its maintenance.lock during fs::copy"
    );
    assert_eq!(read(fixture.repo.join("README.md")), "base\n");
    assert_eq!(git(&fixture.repo, &["config", "user.name"]), "Orbit Test");
    assert_eq!(
        git(&fixture.repo, &["remote", "get-url", "origin"]),
        path_str(&fixture.root.path().join("origin.git")),
        "each row's remote configuration must name its own bare repository"
    );
    let head = head(&earlier.repo);
    let object = earlier
        .repo
        .join(".git/objects")
        .join(&head[..2])
        .join(&head[2..]);
    // Git can make loose objects read-only. Corrupt this row's file
    // deliberately to detect shared inodes, rather than just unlinking it.
    fs::set_permissions(&object, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(object, b"damaged earlier row object").unwrap();
    git(&fixture.repo, &["cat-file", "-e", "HEAD"]);
    git(&fixture.repo, &["fsck", "--strict"]);
    drop(earlier);
    let host = LifecycleHost::new(&fixture.repo);
    let (_, checkout) = setup_checkout(&host, "T-REPEAT", "jrun-repeat-setup");
    let primary = fixture.repo.display().to_string();
    let provider = install_provider(
        fixture.root.path(),
        &format!("printf 'primary drift\\n' > '{primary}/README.md'\n"),
    );
    let host = host.with_provider(&provider);

    let mut roots = Vec::new();
    for failure in 1..=FAILURES {
        git(&fixture.repo, &["checkout", "--", "README.md"]);
        fs::write(
            checkout.path.join(format!("edit-{failure}.txt")),
            format!("edit {failure}\n"),
        )
        .unwrap();
        let error = dispatch_audited_linked_activity(
            &host,
            "agent_implement",
            "jrun-repeat",
            "T-REPEAT",
            &checkout.path,
            audit_writer("jrun-repeat", fixture.root.path()).1,
        )
        .expect_err("primary drift is a boundary failure");
        let diagnostic = integrity_diagnostic(&error, "primary_checkout_drift");
        let recovery = &diagnostic["recovery"];
        assert!(
            recovery.get("preservation_error").is_none(),
            "failure {failure}: {diagnostic}"
        );
        let root = PathBuf::from(recovery["root"].as_str().expect("recovery root"));
        let payload = PathBuf::from(recovery["untracked_payload"].as_str().unwrap());
        let edit = read(payload.join(format!("edit-{failure}.txt")));
        let message = "must preserve the edits present at that failure";
        assert_eq!(
            edit,
            format!("edit {failure}\n"),
            "failure {failure} {message}"
        );
        assert!(
            !roots.contains(&root),
            "failure {failure} reused an earlier payload: {}",
            root.display()
        );
        roots.push(root);
    }

    let run_dir = roots[0].parent().unwrap();
    let kept = fs::read_dir(run_dir).unwrap().count();
    assert!(
        kept < FAILURES,
        "recovery payloads of one run must be bounded, found {kept}"
    );
    assert!(
        roots.last().unwrap().is_dir(),
        "the newest payload is never pruned"
    );
    assert!(
        !roots[0].exists(),
        "the oldest payload is pruned past the retention bound"
    );
}

fn recover(
    host: &LifecycleHost,
    run_id: &str,
    input: Value,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let blobs = TempDir::new().unwrap();
    let spec = agent_loop("Resolve the stopped rebase.");
    dispatch_v2_activity(V2DispatchInput {
        activity_name: "pr_conflict_recovery",
        spec: &spec,
        fs_profile: None,
        input,
        audit: audit_writer(run_id, blobs.path()).1,
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
    let (_, audit) = audit_writer(run_id, blobs.path());
    dispatch_audited_linked_activity(host, activity_name, run_id, task_id, workspace, audit)
}

/// Keep the caller's audit sink available to inspect full boundary evidence.
fn dispatch_audited_linked_activity(
    host: &LifecycleHost,
    activity_name: &str,
    run_id: &str,
    task_id: &str,
    workspace: &Path,
    audit: Arc<V2AuditWriter>,
) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
    let spec = agent_loop("Edit the assigned checkout.");
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

/// A one-iteration Codex agent loop that requires the completion envelope.
fn agent_loop(instruction: &str) -> ActivityV2Spec {
    ActivityV2Spec::AgentLoop(AgentLoopSpec {
        tool_disallow_list: None,
        instruction: instruction.to_string(),
        steering_instruction: None,
        regression_risk_instruction: None,
        explore_instruction: None,
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
        crew_env: Vec::new(),
        crew_account: None,
    })
}

/// An audit writer for `run_id` whose blobs land in `sink` under `blobs`.
fn audit_writer(run_id: &str, blobs: &Path) -> (Arc<InMemorySink>, Arc<V2AuditWriter>) {
    let sink = Arc::new(InMemorySink::new(blobs.to_path_buf()));
    let audit = V2AuditWriter::new(run_id, "codex:test-model", sink.clone());
    (sink, Arc::new(audit))
}

/// The checkout fingerprints an integrity `diagnostic` stored in `sink`.
fn fingerprints(sink: &InMemorySink, diagnostic: &Value) -> Value {
    let blob = diagnostic["fingerprints_blob_ref"].as_str().unwrap();
    serde_json::from_slice(&sink.blob_store().read(blob).unwrap()).unwrap()
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
        thread_local! {
            static TEMPLATE: std::cell::RefCell<Option<Fixture>> = const { std::cell::RefCell::new(None) };
        }
        TEMPLATE.with(|slot| {
            let mut template = slot.borrow_mut();
            let seed = template.get_or_insert_with(Self::fresh);
            let root = TempDir::new().unwrap();
            super::git_fixture::copy_template(seed.root.path(), root.path());
            let repo = canonical(&root.path().join("repo"));
            Self { root, repo }
        })
    }

    fn fresh() -> Self {
        let root = TempDir::new().unwrap();
        let repo = root.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        super::git_fixture::init(&repo);
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
        Self::with_commits(
            run_id,
            &[(candidate_file, "candidate\n")],
            &[(base_file, "target\n")],
        )
    }

    /// A candidate of one commit per `candidate` file write, prepared against
    /// a base advanced by one commit per `base` file write.
    fn with_commits(run_id: &str, candidate: &[(&str, &str)], base: &[(&str, &str)]) -> Self {
        let commit_all = |repo: &Path, writes: &[(&str, &str)]| {
            writes
                .iter()
                .fold(None, |_, (file, contents)| {
                    Some(commit_file(repo, file, contents))
                })
                .expect("at least one commit")
        };
        Self::with_edits(
            run_id,
            |checkout| commit_all(checkout, candidate),
            |repo| commit_all(repo, base),
        )
    }

    /// A candidate committed by `candidate` in the run's checkout, prepared
    /// against a base `base` advanced in the primary; each returns its tip.
    fn with_edits(
        run_id: &str,
        candidate: impl FnOnce(&Path) -> String,
        base: impl FnOnce(&Path) -> String,
    ) -> Self {
        let fixture = Fixture::new();
        Self::with_fixture(run_id, fixture, candidate, base)
    }

    /// Prepare from a fixture with a caller-selected common ancestor.
    fn with_fixture(
        run_id: &str,
        fixture: Fixture,
        candidate: impl FnOnce(&Path) -> String,
        base: impl FnOnce(&Path) -> String,
    ) -> Self {
        let host = LifecycleHost::new(&fixture.repo);
        let (setup, checkout) = setup_checkout(&host, "T-REBASE", run_id);
        let base_sha = setup["base_sha"].as_str().unwrap().to_string();
        let candidate = candidate(&checkout.path);
        let target = base(&fixture.repo);
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
        self.rebase_on(&self.host, &self.prepared)
    }

    /// Run `git_rebase` through `host` for the handoff `preparation` describes.
    fn rebase_on(&self, host: &LifecycleHost, preparation: &Value) -> Result<Value, OrbitError> {
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
            input[field] = preparation[field].clone();
        }
        action(host, "git_rebase", &input)
    }

    fn head(&self) -> String {
        head(&self.checkout.path)
    }

    /// This fixture's host with a substitute `codex` that runs `body` in
    /// the checkout.
    #[cfg(unix)]
    fn with_provider(&self, body: &str) -> LifecycleHost {
        let provider = install_provider(self.fixture.root.path(), body);
        self.host.with_provider(&provider)
    }

    /// Run conflict recovery on `host` for this fixture's stopped rebase.
    #[cfg(unix)]
    fn recover_conflict(
        &self,
        host: &LifecycleHost,
        conflict: &Value,
    ) -> Result<orbit_engine::DispatchOutcome, DispatchError> {
        recover(host, &self.run_id, conflict_recovery_input(self, conflict))
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

/// Admit `task_id` from the backlog and set up `run_id`'s checkout for it.
fn setup_checkout(host: &LifecycleHost, task_id: &str, run_id: &str) -> (Value, Checkout) {
    host.add_task(task_id, TaskStatus::Backlog);
    let setup = action(host, "worktree_setup", &setup_input(&[task_id], run_id))
        .unwrap_or_else(|error| panic!("worktree setup for {run_id}: {error}"));
    let checkout = Checkout::from_setup(&setup);
    (setup, checkout)
}

/// A successful run's checkout as GC finds it, its task then at `status`
/// (still admitted when `None`).
fn gc_checkout(
    host: &LifecycleHost,
    run_id: &str,
    task_id: &str,
    status: Option<TaskStatus>,
) -> Checkout {
    let (_, checkout) = setup_checkout(host, task_id, run_id);
    if let Some(status) = status {
        host.set_status(task_id, status);
    }
    let input = setup_input(&[task_id], run_id);
    host.add_run(job_run(run_id, JobRunState::Success, input));
    checkout
}

fn gc(host: &LifecycleHost, input: Value) -> Value {
    action(host, "worktree_gc", &input).expect("worktree gc")
}

/// The report a `worktree_gc` result gives for `run_id`.
fn gc_report<'a>(result: &'a Value, run_id: &str) -> &'a Value {
    let reports = result["reports"].as_array().expect("gc reports");
    reports
        .iter()
        .find(|report| report["run_id"] == run_id)
        .unwrap_or_else(|| panic!("{run_id}: no gc report in {result:#}"))
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
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        origin: None,
        created_at: now,
        updated_at: now,
    }
}

/// The task store, run store and provider resolution the lifecycle actions
/// read through, kept in memory.
#[derive(Default)]
struct LifecycleHost {
    worker: Option<orbit_types::tool::WorkerInvocation>,
    gate_source: Option<orbit_types::workflow::ValidationGateSource>,
    reclaim_patterns: Option<Vec<String>>,
    repo: PathBuf,
    provider: Option<PathBuf>,
    tasks: Mutex<BTreeMap<String, Task>>,
    runs: Mutex<Vec<JobRun>>,
    admitted: Mutex<Vec<String>>,
    checkpoints: Mutex<Vec<(String, String, Value)>>,
    /// Admitted conflict recoveries, as (run, step, scope), in reservation
    /// order. The attempt is a recovery's 1-based position for its run/step.
    recovery_attempts: Mutex<Vec<(String, String, RebaseRecoveryAttemptScope)>>,
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
    /// Task artifacts by task id.
    artifacts: Mutex<BTreeMap<String, Vec<TaskArtifact>>>,
    /// This host's machine identity.
    machine: Mutex<Option<String>>,
    /// The candidate the owner kept from the task's last claim.
    kept_claim: Mutex<Option<KeptClaimCandidate>>,
    /// Deterministically model an operator write immediately before landing.
    landing_write: Mutex<Option<(PathBuf, Vec<u8>)>>,
    /// Scripted answers of host-run deterministic actions, by action name.
    stubs: Mutex<BTreeMap<String, Result<Value, DispatchError>>>,
    /// Every stubbed action call, as (action, input), in order.
    stub_calls: Mutex<Vec<(String, Value)>>,
    /// Final recovery admissions requested, in order.
    final_recovery_admissions: Mutex<Vec<FinalRecoveryAdmissionRequest>>,
    /// Final recovery decisions handed to the host, in order.
    final_recovery_applications: Mutex<Vec<FinalRecoveryApplication>>,
    /// Why the live run/task/worktree no longer owns a step recovery.
    recovery_owner_refusal: Mutex<Option<String>>,
    /// Why step recovery authorization was revoked.
    recovery_revocation: Mutex<Option<String>>,
    /// Deterministic race after host certification and before synchronization.
    preparation_hook: Mutex<Option<PreparationHook>>,
    /// `workflow.unlocked_context_paths` with
    /// `workflow.unlocked_path_validation_commands`.
    unlocked_validation: Mutex<orbit_engine::UnlockedPathValidation>,
    /// `workflow.generated_paths`.
    generated_paths: Mutex<Vec<orbit_engine::GeneratedPath>>,
    /// The host's record of each run's last fully validated tree.
    validated_trees: Mutex<BTreeMap<String, orbit_engine::ValidatedTree>>,
    /// `[pr]`, `[commit]` and `[branch]` delivery settings.
    pr_config: Mutex<orbit_engine::PrConfig>,
}

type Widening = (String, ContextWideningStep, String, Vec<String>);
type PreparationHook = Box<dyn FnMut(&Value) + Send>;

impl LifecycleHost {
    pub(super) fn stub(&self, action: &str, reply: Result<Value, String>) {
        self.stubs.lock().unwrap().insert(
            action.to_string(),
            reply.map_err(|message| DispatchError::action_failed(action, message)),
        );
    }

    pub(super) fn stub_calls_of(&self, action: &str) -> Vec<Value> {
        self.stub_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == action)
            .map(|(_, input)| input.clone())
            .collect()
    }

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
            unlocked_validation: Mutex::new(self.unlocked_validation.lock().unwrap().clone()),
            generated_paths: Mutex::new(self.generated_paths.lock().unwrap().clone()),
            ..Self::default()
        }
    }

    fn add_task(&self, id: &str, status: TaskStatus) {
        self.tasks
            .lock()
            .unwrap()
            .insert(id.to_string(), task(id, status));
    }

    fn edit_task(&self, id: &str, edit: impl FnOnce(&mut Task)) {
        edit(self.tasks.lock().unwrap().get_mut(id).unwrap());
    }

    fn set_status(&self, id: &str, status: TaskStatus) {
        self.edit_task(id, |task| task.status = status);
    }

    fn link_run(&self, id: &str, run_id: &str) {
        self.edit_task(id, |task| task.job_run_id = Some(run_id.to_string()));
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

    fn recovery_attempts(&self) -> Vec<(String, String, RebaseRecoveryAttemptScope)> {
        self.recovery_attempts.lock().unwrap().clone()
    }

    /// Overwrite the run store's copy of `step_id`'s recovery, as a leaf
    /// holding the store's modify grant can.
    fn leaf_writes_recovery(&self, run_id: &str, step_id: &str, checkpoint: Value) {
        self.run_states
            .lock()
            .unwrap()
            .get_mut(run_id)
            .unwrap()
            .rebase_recovery_checkpoints
            .insert(step_id.to_string(), checkpoint);
    }

    fn widenings(&self) -> Vec<Widening> {
        self.widenings.lock().unwrap().clone()
    }

    fn set_description(&self, id: &str, description: &str) {
        self.edit_task(id, |task| task.description = description.to_string());
    }

    fn set_acceptance_criteria(&self, id: &str, criteria: &[&str]) {
        let criteria = criteria.iter().map(ToString::to_string).collect();
        self.edit_task(id, |task| task.acceptance_criteria = criteria);
    }

    fn set_context_files(&self, id: &str, selectors: &[&str]) {
        let selectors = selectors.iter().map(ToString::to_string).collect();
        self.edit_task(id, |task| task.context_files = selectors);
    }

    /// Store `artifact` on `task_id`, replacing one at the same path.
    fn set_artifact(&self, task_id: &str, artifact: TaskArtifact) {
        let mut artifacts = self.artifacts.lock().unwrap();
        let stored = artifacts.entry(task_id.to_string()).or_default();
        stored.retain(|existing| existing.path != artifact.path);
        stored.push(artifact);
    }

    fn set_unlocked_validation(&self, paths: &[&str], commands: &[&str]) {
        *self.unlocked_validation.lock().unwrap() = orbit_engine::UnlockedPathValidation {
            paths: paths.iter().map(ToString::to_string).collect(),
            commands: commands.iter().map(ToString::to_string).collect(),
        };
    }

    fn set_generated_path(&self, path: &str, command: &str) {
        *self.generated_paths.lock().unwrap() = vec![orbit_engine::GeneratedPath {
            path: path.to_string(),
            command: command.to_string(),
        }];
    }

    fn set_required_commands(&self, commands: &[&str]) {
        *self.required_commands.lock().unwrap() =
            commands.iter().map(ToString::to_string).collect();
    }

    /// Record `output` as `run_id`'s failure-activity checkpoint.
    fn preserve(&self, run_id: &str, failed_step_id: &str, output: Value) {
        let mut state = pipeline_state(run_id);
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

fn conflict_recovery_activity() -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": "pr_conflict_recovery" },
        "spec": {
            "type": "agent_loop",
            "description": "Resolve the stopped rebase.",
            "instruction": "Resolve the stopped rebase.",
            "provider": "codex",
            "max_iterations": 1,
            "wall_clock_timeout_seconds": 30,
            "require_completion_envelope": true,
            "on_denial": "terminate",
        },
    });
    orbit_engine::activity_job::load_activity_asset(&asset.to_string())
        .expect("conflict recovery fixture loads")
        .spec
}

fn deterministic_activity(name: &str) -> ActivityV2 {
    let asset = json!({
        "schemaVersion": 2,
        "kind": "Activity",
        "metadata": { "name": name },
        "spec": { "type": "deterministic", "description": name, "action": name, "config": {} },
    });
    orbit_engine::activity_job::load_activity_asset(&asset.to_string())
        .expect("fixture activity loads")
        .spec
}

impl orbit_engine::test_support::NullRuntimeHost for LifecycleHost {
    fn pr_config(&self) -> orbit_engine::PrConfig {
        self.pr_config.lock().unwrap().clone()
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        let mut environment = orbit_exec::ValidationEnvironment::launcher(
            RuntimeHost::agent_subprocess_environment(self, &[]),
        );
        environment.build_output_env = vec!["BUILD_OUTPUT_DIR".into()];
        environment
    }

    fn record_direct_landing_intent(
        &self,
        _request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        if let Some((path, bytes)) = self.landing_write.lock().unwrap().take() {
            fs::write(path, bytes).unwrap();
        }
        Ok(())
    }
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
        RuntimeHost::get_task(self, task_id)
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
            let entry = TaskHistoryEntry {
                note: update.status_note,
                ..history_entry(Utc::now(), "system", &event)
            };
            self.record_history(task_id, entry);
        }
        Ok(())
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(self.history(task_id))
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        let artifact = self
            .artifacts
            .lock()
            .unwrap()
            .get(task_id)
            .and_then(|artifacts| artifacts.iter().find(|artifact| artifact.path == path))
            .cloned();
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(task_id)?;
        }
        Ok(artifact)
    }

    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .get(task_id)
            .cloned()
            .unwrap_or_default())
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

    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        orbit_types::workflow::HostValidationGates {
            gates: orbit_types::workflow::ValidationGates {
                required_validation_commands: self.required_commands.lock().unwrap().clone(),
                baseline_commands: Vec::new(),
                light_delivery_paths: None,
            },
            source: self
                .gate_source
                .unwrap_or(orbit_types::workflow::ValidationGateSource::LocalConfig),
        }
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        self.worker.clone()
    }

    fn unlocked_path_validation(&self) -> orbit_engine::UnlockedPathValidation {
        self.unlocked_validation.lock().unwrap().clone()
    }

    fn generated_paths(&self) -> Vec<orbit_engine::GeneratedPath> {
        self.generated_paths.lock().unwrap().clone()
    }

    fn record_validated_tree(&self, tree: &orbit_engine::ValidatedTree) -> Result<(), OrbitError> {
        self.validated_trees
            .lock()
            .unwrap()
            .insert(tree.run_id.clone(), tree.clone());
        Ok(())
    }

    fn validated_tree(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_engine::ValidatedTree>, OrbitError> {
        Ok(self.validated_trees.lock().unwrap().get(run_id).cloned())
    }

    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        Ok(())
    }

    fn register_worker_pid_namespace(&self, _pid: u32) -> Result<(), OrbitError> {
        Ok(())
    }

    fn claim_execution_context(&self) -> Result<orbit_engine::ClaimExecutionContext, OrbitError> {
        let worker = self.worker.as_ref().expect("claimed fixture");
        Ok(orbit_engine::ClaimExecutionContext {
            footprint: vec!["file:README.md".to_string()],
            workspace_id: worker.owner_workspace_id.clone(),
            task_id: worker.task_id.clone(),
            claim_id: worker.claim_id.clone(),
            machine_id: worker.execution.machine_id.clone(),
            run_id: worker.bound_run_id.clone(),
            ship_mode: "pr".to_string(),
            base_branch: "agent-main".to_string(),
            landing_branch: "agent-main".to_string(),
            required_commands: self.required_commands.lock().unwrap().clone(),
        })
    }

    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        assert_eq!(
            RuntimeHost::get_task(self, task_id)?.job_run_id.as_deref(),
            Some(run_id)
        );
        let mut artifacts = self.artifacts.lock().unwrap();
        let artifacts = artifacts.entry(task_id.to_string()).or_default();
        artifacts.retain(|artifact| artifact.path != path);
        artifacts.push(TaskArtifact::from_text(
            path,
            String::from_utf8(content).unwrap(),
        ));
        Ok(())
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        let worker = self
            .worker
            .as_ref()
            .expect("claim routes evidence to its owner");
        RuntimeHost::attach_task_validation_log(
            self,
            &worker.task_id,
            &worker.bound_run_id,
            path,
            content,
        )
    }

    fn local_machine_id(&self) -> Option<String> {
        self.machine.lock().unwrap().clone()
    }

    fn kept_claim_candidate(
        &self,
        _task_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        Ok(self.kept_claim.lock().unwrap().clone())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo.to_string_lossy().into_owned())
    }

    fn worktree_reclaim_patterns(&self) -> Vec<String> {
        // Unset means the shipped default, as on a host without configuration.
        self.reclaim_patterns.clone().unwrap_or_else(|| {
            ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]
                .map(String::from)
                .to_vec()
        })
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(self.runs.lock().unwrap().clone())
    }

    fn lookup_task_for_worktree_gc(&self, _run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        if let Some(answer) = self.owner_answers.lock().unwrap().get(task_id) {
            return answer.clone();
        }
        match RuntimeHost::get_task(self, task_id) {
            Ok(task) => WorktreeGcTaskLookup::Found {
                status: task.status,
                pr_status: task.pr_status,
                status_since: Some(task.updated_at),
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

    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        let mut attempts = self.recovery_attempts.lock().unwrap();
        attempts.push((run_id.to_string(), step_id.to_string(), scope.clone()));
        Ok(attempts
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .count() as u64)
    }

    /// Records the certified completion, and copies it into the run store the
    /// `git_rebase` retry reads, as the runtime does.
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
        self.run_states
            .lock()
            .unwrap()
            .entry(run_id.to_string())
            .or_insert_with(|| pipeline_state(run_id))
            .rebase_recovery_checkpoints
            .insert(step_id.to_string(), output.clone());
        if let Some(hook) = self.preparation_hook.lock().unwrap().as_mut() {
            hook(output);
        }
        Ok(())
    }

    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        Ok(self
            .recovery_attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|(run, step, _)| run == run_id && step == step_id)
            .map(|(_, _, scope)| scope.clone())
            .collect())
    }

    /// Only the newest completion recorded for a run's step vouches for it,
    /// matching the runtime's recovery authority.
    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        Ok(self
            .checkpoints
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(run, step, _)| run == run_id && step == step_id)
            .is_some_and(|(_, _, certified)| certified == checkpoint))
    }

    fn validate_step_recovery_mutation(
        &self,
        _run_id: &str,
        _step_id: &str,
        _task_ids: &[String],
        _workspace_path: &Path,
    ) -> Result<(), OrbitError> {
        match self.recovery_owner_refusal.lock().unwrap().clone() {
            Some(reason) => Err(OrbitError::Execution(reason)),
            None => Ok(()),
        }
    }

    fn authorize_step_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        Ok(match self.recovery_revocation.lock().unwrap().clone() {
            Some(reason) => StepRecoveryAdmission::Denied { reason },
            None => StepRecoveryAdmission::Allowed,
        })
    }

    fn run_deterministic(
        &self,
        action: &str,
        _config: &Value,
        input: &Value,
        _tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        let Some(reply) = self.stubs.lock().unwrap().get(action).cloned() else {
            return Err(DispatchError::DeterministicActionNotRegistered(
                action.to_string(),
            ));
        };
        self.stub_calls
            .lock()
            .unwrap()
            .push((action.to_string(), input.clone()));
        reply
    }

    /// Conflict recovery dispatched by a job draws from a complexity pool;
    /// with no crew registry here it keeps the activity's inline settings.
    fn agent_crew_config_for_input(
        &self,
        _input: &Value,
    ) -> Result<Option<orbit_engine::CrewConfig>, DispatchError> {
        Ok(None)
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.final_recovery_admissions
            .lock()
            .unwrap()
            .push(request.clone());
        Ok(FinalRecoveryAdmission::Admitted)
    }

    /// Settles every decision but `escalate`, leaving the task as it is: the
    /// runtime's applier, not this host, owns the lifecycle change.
    fn apply_final_recovery(
        &self,
        _run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        self.final_recovery_applications
            .lock()
            .unwrap()
            .push(application.clone());
        Ok(match &application.decision {
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated {
                outcome: "blocked for a human".to_string(),
            },
            other => FinalRecoveryApplied::Settled {
                outcome: other.kind().to_string(),
            },
        })
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        let command = self
            .provider
            .as_ref()
            .unwrap_or_else(|| panic!("no substitute CLI configured for provider {provider}"));
        Ok(ResolvedCliExecutor {
            command: command.to_string_lossy().into_owned(),
            args: Vec::new(),
            inspection_args: None,
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

fn assert_landing_checkout_refusal(error: &OrbitError, landing: &Path, spelling: &str) {
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::Execution(_)),
        "base spelling {spelling} should refuse at the landing checkout, got {error:?}"
    );
    assert!(
        message.contains("base branch checkout"),
        "base spelling {spelling} names the landing-checkout check: {message}"
    );
    assert!(
        message.contains(&landing.display().to_string()),
        "base spelling {spelling} inspects the checkout holding main ({}): {message}",
        landing.display()
    );
    assert!(
        message.contains("README.md")
            && message.contains("must be clean before merge_batch_worktree_into_base"),
        "base spelling {spelling} reports that checkout's dirty path: {message}"
    );
    assert!(
        !message.contains("unrelated.txt"),
        "base spelling {spelling} leaves the unrelated primary checkout out of the refusal: {message}"
    );
}

fn checkout_holding(repo: &Path, branch: &str) -> PathBuf {
    let listing = git(repo, &["worktree", "list", "--porcelain"]);
    let expected = format!("refs/heads/{branch}");
    let mut current = None;
    for line in listing.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if line.strip_prefix("branch ") == Some(expected.as_str()) {
            return current.unwrap_or_else(|| panic!("worktree path missing for {branch}"));
        }
    }
    panic!("no checkout holds {branch}:\n{listing}");
}

/// The dispatcher injects `run_id` beside the pipeline's `job_run_id`
/// before `git_merge` runs. Both name this run.
fn merge_input(run_id: &str, spelling: &str, workspace: &Path) -> Value {
    json!({
        "run_id": run_id,
        "job_run_id": run_id,
        "base": spelling,
        "base_sync": "local",
        "strategy": "fast_forward",
        "workspace_path": workspace,
    })
}

fn merge(
    host: &LifecycleHost,
    run_id: &str,
    spelling: &str,
    workspace: &Path,
) -> Result<Value, OrbitError> {
    action(host, "git_merge", &merge_input(run_id, spelling, workspace))
}

/// `T-LAND`'s local-landing checkout for `run_id` onto `base`.
fn landing_checkout(host: &LifecycleHost, base: &str, run_id: &str) -> Checkout {
    host.add_task("T-LAND", TaskStatus::Backlog);
    let input = landing_input("base", base, run_id);
    let setup = action(host, "worktree_setup", &input)
        .unwrap_or_else(|error| panic!("local setup for {run_id}: {error}"));
    Checkout::from_setup(&setup)
}

fn landing_input(field: &str, spelling: &str, run_id: &str) -> Value {
    json!({
        "task_ids": ["T-LAND"],
        "run_id": run_id,
        "base_sync": "local",
        "dependency_delivery": "ignore",
        "landing_mode": "local",
        field: spelling,
    })
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
    crate::git_test_support::command()
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .status()
        .unwrap()
        .success()
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    super::git_fixture::run(current_dir, args)
}

fn head(checkout: &Path) -> String {
    git(checkout, &["rev-parse", "HEAD"])
}

fn branch(checkout: &Path) -> String {
    git(checkout, &["rev-parse", "--abbrev-ref", "HEAD"])
}

fn status(checkout: &Path) -> String {
    git(checkout, &["status", "--porcelain"])
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The checkout a `worktree_setup` output names.
fn workspace(setup: &Value) -> PathBuf {
    Checkout::from_setup(setup).path
}

fn pipeline_state(run_id: &str) -> PipelineState {
    PipelineState::new(
        run_id.to_string(),
        "task_pr_pipeline".to_string(),
        json!({}),
    )
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
    isolated_in(module_path!(), test, body);
}

/// [`isolated`] for a test defined in `module`, a submodule of this one.
fn isolated_in(module: &str, test: &str, body: impl FnOnce()) {
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
        module.split_once("::").expect("test module").1
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([&qualified, "--exact", "--nocapture", "--test-threads=1"])
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    // Stream row progress into nextest, including when it terminates the parent.
    if module.ends_with("::absorbed_candidate") {
        command.stderr(Stdio::inherit());
    }
    for (name, _) in std::env::vars_os() {
        let name = name.to_string_lossy();
        if name.starts_with("ORBIT_") || name.starts_with("GIT_") {
            command.env_remove(name.as_ref());
        }
    }
    super::git_fixture::configure_child(&mut command, &home, sandbox.path());
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

/// `run_scenarios` rows named by their scenario function, keeping each
/// row's `cfg` gate.
macro_rules! scenarios {
    ($($(#[$gate:meta])* $scenario:ident),* $(,)?) => {
        &[$($(#[$gate])* (stringify!($scenario), $scenario as fn())),*]
    };
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_1() {
    isolated("worktree_contracts_1", || {
        crate::git_fixture::run_scenarios(scenarios![
            worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones,
            replica_worktree_gc_reclaims_settled_claims_and_says_why_it_keeps_the_rest,
            replica_worktree_gc_fences_unreachable_owners_by_route,
            worktree_gc_classifies_other_worktrees_when_a_run_id_sanitizes_empty,
            worktree_setup_keeps_a_branch_prefix_checkout_under_the_worktree_root,
            worktree_gc_rejects_non_string_target_run_id_and_deletes_nothing,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_2() {
    isolated("worktree_contracts_2", || {
        crate::git_fixture::run_scenarios(scenarios![
            worktree_setup_refuses_a_stale_checkout_and_leaves_it_untouched,
            worktree_setup_names_a_templated_branch_stably_and_refuses_an_unsafe_render,
            worktree_setup_refuses_a_checkout_with_unreadable_status_and_keeps_its_edits,
            local_landing_precheck_uses_the_normalized_base_checkout,
            local_landing_preserves_unrelated_untracked_files,
            local_landing_names_only_untracked_paths_conflicting_with_the_candidate,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_3() {
    isolated("worktree_contracts_3", || {
        crate::git_fixture::run_scenarios(scenarios![
            local_landing_still_refuses_tracked_and_unmerged_work_at_setup_and_merge,
            a_validated_candidate_is_resumed_on_the_new_base_without_implementation,
            a_candidate_held_for_an_upgrade_refusal_is_resumed_by_the_next_run,
            a_candidate_held_for_a_wait_timeout_is_resumed_by_the_next_run,
            a_conflicting_candidate_is_handed_to_the_implementer_with_its_conflict,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_4() {
    isolated("worktree_contracts_4", || {
        crate::git_fixture::run_scenarios(scenarios![
            a_candidate_failing_validation_is_handed_to_the_implementer_with_the_output,
            a_candidate_failing_validation_its_base_shares_is_not_handed_to_the_implementer,
            an_implementation_failure_handoff_is_requeued_for_the_implementer,
            resumed_known_red_candidates_must_complete_host_and_replay_coverage,
            a_changed_spec_or_an_operator_discard_implements_fresh,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_5() {
    isolated("worktree_contracts_5", || {
        crate::git_fixture::run_scenarios(scenarios![
            a_selector_only_edit_keeps_the_preserved_candidate,
            an_evidence_receipt_resumes_the_held_candidate_without_the_implementer,
            a_held_candidate_that_is_gone_or_retired_implements_fresh_with_a_typed_reason,
            a_claimed_leaf_continues_the_candidate_its_owner_kept,
            a_local_claim_leaf_falls_back_to_the_candidate_its_claim_kept,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_6() {
    isolated("worktree_contracts_6", || {
        crate::git_fixture::run_scenarios(scenarios![
            a_repair_claim_resumes_its_stopped_landing_candidate_for_the_implementer,
            git_rebase_decision_follows_the_checkout_state,
            git_rebase_rename_delete_conflicts_remain_for_recovery,
            #[cfg(unix)]
            conflict_recovery_leaf_completes_only_its_checkpointed_rebase,
            #[cfg(unix)]
            a_resumed_run_recovers_the_same_step_again_as_a_new_attempt,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_7() {
    isolated("worktree_contracts_7", || {
        crate::git_fixture::run_scenarios(scenarios![
            #[cfg(unix)]
            conflict_recovery_commits_companion_edits_with_the_resolution,
            #[cfg(unix)]
            conflict_recovery_resolves_each_stop_of_a_multi_commit_rebase,
            #[cfg(unix)]
            conflict_recovery_still_refuses_unresolved_conflicts_beside_companion_edits,
            #[cfg(unix)]
            agent_changed_paths_widen_selectors_with_their_step_as_the_agent_exits,
            #[cfg(unix)]
            provider_primary_source_edit_fails_closed_and_preserves_both_checkouts,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_8() {
    isolated("worktree_contracts_8", || {
        crate::git_fixture::run_scenarios(scenarios![
            #[cfg(unix)]
            user_git_diff_config_preserves_dirty_fingerprints_and_recovery,
            #[cfg(unix)]
            stationary_record_store_dirt_overlapping_the_run_fails_closed,
            #[cfg(unix)]
            stationary_record_store_dirt_disjoint_from_the_run_stays_benign,
            #[cfg(unix)]
            primary_dirtied_between_dispatches_of_one_run_is_not_provider_drift,
            #[cfg(unix)]
            untracked_nested_repository_fingerprints_in_primary_and_assigned_worktree,
        ]);
    });
}

/// Behavior-distinct rows share one bounded family process.
#[test]
fn worktree_contracts_9() {
    isolated("worktree_contracts_9", || {
        crate::git_fixture::run_scenarios(scenarios![
            #[cfg(unix)]
            repeated_dirty_integrity_failures_keep_current_content_and_prune_old_attempts,
        ]);
    });
}
