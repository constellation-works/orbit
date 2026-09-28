#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{Duration, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::context::{RuntimeHost, WorktreeGcTaskLookup};

use super::super::cleanup::{
    recover_timed_out_removal, remove_worktree, remove_worktree_without_force,
};
use super::super::gc::{WorktreeGcOptions, WorktreeGcReport, collect_worktrees};
use super::super::{
    WorktreeIdentity, is_registered_worktree, resolve_shared_worktree_path,
    resolve_worktree_path_from_prefix,
};
use crate::executor::automation::vcs::git::{GitTimeoutBudget, GitTimeoutBudgetGuard};

// Environment variables are process-global: mutating ORBIT_WORKTREE_ROOT in
// this parallel test binary races every test that resolves a worktree path.
// Run the two environment-specific cases in isolated copies of the test
// process so the rest of the module stays parallel without observing them.
const RESOLVER_ENV_CHILD: &str = "ORBIT_GC_RESOLVER_ENV_CHILD";

struct FakeTaskHost {
    tasks: BTreeMap<String, Task>,
}

impl FakeTaskHost {
    fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks: tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
        }
    }
}

impl RuntimeHost for FakeTaskHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
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
        Ok(self.tasks.values().cloned().collect())
    }
}

/// A replica checkout: it holds no task records of its own, and every GC
/// lookup goes to a stubbed owner that either answers from `owner_tasks` or,
/// when that is `None`, cannot be reached.
struct ReplicaHost {
    owner_tasks: Option<BTreeMap<String, TaskStatus>>,
    owner_calls: AtomicUsize,
}

impl ReplicaHost {
    fn owner_answers(tasks: &[(&str, TaskStatus)]) -> Self {
        Self {
            owner_tasks: Some(
                tasks
                    .iter()
                    .map(|(id, status)| (id.to_string(), *status))
                    .collect(),
            ),
            owner_calls: AtomicUsize::new(0),
        }
    }

    fn owner_unreachable() -> Self {
        Self {
            owner_tasks: None,
            owner_calls: AtomicUsize::new(0),
        }
    }
}

impl RuntimeHost for ReplicaHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        Err(OrbitError::not_found(
            NotFoundKind::Task,
            task_id.to_string(),
        ))
    }

    fn lookup_task_for_worktree_gc(&self, task_id: &str) -> WorktreeGcTaskLookup {
        self.owner_calls.fetch_add(1, Ordering::SeqCst);
        match &self.owner_tasks {
            None => WorktreeGcTaskLookup::OwnerUnreachable("ssh: connect timed out".to_string()),
            Some(tasks) => tasks
                .get(task_id)
                .map_or(WorktreeGcTaskLookup::Unresolved, |status| {
                    WorktreeGcTaskLookup::Found {
                        status: *status,
                        pr_status: None,
                    }
                }),
        }
    }
}

fn task_fixture(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: "fixture task".to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
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

#[test]
fn unrecognized_hand_made_worktree_survives_collection() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let hand_made = repo
        .join(".orbit/state/worktrees")
        .join("orbit-ORB-10354-part2");
    fs::create_dir_all(&hand_made).unwrap();
    fs::write(hand_made.join("rescue.txt"), "keep me").unwrap();

    let host = FakeTaskHost::new(Vec::new());
    let result = collect_worktrees(
        &repo,
        &[],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(hand_made.exists());
    let report = result
        .reports
        .iter()
        .find(|report| report.path == hand_made)
        .expect("unrecognized report");
    assert_eq!(report.action, "skipped:unrecognized");
    assert_eq!(report.run_id, None);
}

#[test]
fn terminal_dirty_worktree_is_a_rescue_candidate() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-dirty", JobRunState::Failed, &["ORB-DIRTY"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/dirty");
    fs::write(worktree.join("uncommitted.txt"), "valuable").unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-DIRTY", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:dirty_rescue_candidate");
    assert_eq!(result.reports[0].bytes_reclaimed, 0);
    assert_eq!(result.reports[0].task_status, Some(TaskStatus::Done));
}

#[test]
fn dry_run_and_yes_share_eligibility_but_only_yes_removes() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-clean", JobRunState::Success, &["ORB-CLEAN"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/clean");
    fs::write(worktree.join("bytes.bin"), [1_u8; 32]).unwrap();
    git(&worktree, &["add", "bytes.bin"]);
    git(&worktree, &["commit", "-m", "worktree content"]);
    let host = FakeTaskHost::new(vec![task_fixture("ORB-CLEAN", TaskStatus::Done)]);

    let dry = collect_worktrees(
        &repo,
        std::slice::from_ref(&run),
        &host,
        &WorktreeGcOptions::default(),
    )
    .unwrap();
    assert!(worktree.exists());
    assert_eq!(dry.reports[0].action, "would_remove");
    assert_eq!(dry.reports[0].task_id.as_deref(), Some("ORB-CLEAN"));
    assert_eq!(
        dry.reports[0].bytes_reclaimed, 0,
        "dry-run skips the directory walk unless estimate_bytes is set"
    );

    let applied = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!worktree.exists());
    assert_eq!(applied.reports[0].action, "removed");
    assert!(applied.reports[0].bytes_reclaimed > 0);
}

/// A branch that cannot be deleted must not turn a completed removal into an
/// error: the directory is gone, its bytes are reclaimed, and the pass keeps
/// going. The report says the branch stayed behind.
#[test]
fn removed_worktree_with_an_undeletable_branch_is_reported_not_failed() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-locked", JobRunState::Success, &["ORB-LOCKED"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/locked");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-LOCKED", TaskStatus::Done)]);
    // A stale ref lock makes `git branch -D` fail while the worktree itself
    // removes cleanly.
    let lock = repo
        .join(".git")
        .join("refs")
        .join("heads")
        .join("orbit")
        .join("locked.lock");
    fs::create_dir_all(lock.parent().unwrap()).unwrap();
    fs::write(&lock, b"").unwrap();

    let applied = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!worktree.exists());
    assert_eq!(applied.reports[0].action, "removed:branch_retained");
    assert!(applied.reports[0].bytes_reclaimed > 0);
    assert!(
        git(&repo, &["branch", "--list", "orbit/locked"]).contains("orbit/locked"),
        "the branch stays until the lock is cleared"
    );
}

#[test]
fn colliding_sanitized_run_ids_are_ambiguous_and_never_removed() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let first = pipeline_run("jrun:collision", JobRunState::Success, &["ORB-COLLISION"]);
    let second = pipeline_run("jrun-collision", JobRunState::Success, &["ORB-COLLISION"]);
    let worktree = resolved_task_worktree(&repo, &first);
    assert_eq!(worktree, resolved_task_worktree(&repo, &second));
    add_worktree(&repo, &worktree, "orbit/collision");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-COLLISION", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[first, second],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports.len(), 2);
    assert!(
        result
            .reports
            .iter()
            .all(|report| report.action == "skipped:ambiguous_run_path")
    );
}

#[test]
fn terminal_run_with_blocked_task_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-blocked", JobRunState::Success, &["ORB-BLOCKED"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/blocked");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-BLOCKED", TaskStatus::Blocked)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:task_status_ineligible");
    assert_eq!(result.reports[0].task_id.as_deref(), Some("ORB-BLOCKED"));
    assert_eq!(result.reports[0].task_status, Some(TaskStatus::Blocked));
}

#[test]
fn terminal_run_with_review_task_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-review", JobRunState::Success, &["ORB-REVIEW"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/review");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-REVIEW", TaskStatus::Review)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:task_status_ineligible");
    assert_eq!(result.reports[0].task_status, Some(TaskStatus::Review));
}

#[test]
fn unattributed_run_is_retained_and_reported() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = unattributed_run("jrun-unattributed", JobRunState::Success);
    let worktree = resolve_shared_worktree_path(&repo, &run.run_id).unwrap();
    add_worktree(&repo, &worktree, "orbit/unattributed");
    let host = FakeTaskHost::new(Vec::new());

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:unattributed");
    assert_eq!(result.reports[0].task_id, None);
    assert_eq!(result.reports[0].task_status, None);
}

#[test]
fn resolver_uses_workspace_local_root_without_override() {
    if !is_resolver_env_child("workspace-local") {
        run_resolver_test_in_child(
            "resolver_uses_workspace_local_root_without_override",
            "workspace-local",
            None,
        );
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let path = resolve_worktree_path_from_prefix(&repo, "orbit", "jrun-local").unwrap();
    assert_eq!(path, repo.join(".orbit/state/worktrees/orbit-jrun-local"));
}

#[test]
fn resolver_uses_external_root_and_repository_name_when_configured() {
    if !is_resolver_env_child("external-root") {
        let temp = tempdir().unwrap();
        let root = temp.path().join("worktrees");
        run_resolver_test_in_child(
            "resolver_uses_external_root_and_repository_name_when_configured",
            "external-root",
            Some(&root),
        );
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("my-repo");
    let root = PathBuf::from(std::env::var_os("ORBIT_WORKTREE_ROOT").unwrap());
    let path = resolve_worktree_path_from_prefix(&repo, "orbit", "jrun-external").unwrap();
    assert_eq!(path, root.join("my-repo/orbit-jrun-external"));
}

/// ORB-10427: the collector reclaimed nothing in production because it probed
/// a singular `task_id` that `task_pr_pipeline` never writes, derived a
/// `parallel-batch-*` path no worktree occupied, and reported every real
/// worktree `skipped:unrecognized`. Nothing about this worktree is
/// unrecognizable: the run record is complete and terminal and its task is
/// settled.
#[test]
fn pipeline_shaped_run_is_recognized_and_reported_in_full() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-20260726-0305-2", JobRunState::Success, &["ORB-10419"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/ORB-10419-6a657983");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-10419", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        std::slice::from_ref(&run),
        &host,
        &WorktreeGcOptions::default(),
    )
    .unwrap();

    let report = result
        .reports
        .iter()
        .find(|report| report.path == worktree)
        .expect("the worktree the pipeline created must appear in the report");
    assert!(
        !report.action.starts_with("skipped:"),
        "an attributable worktree must not be skipped, got {}",
        report.action
    );
    assert_eq!(report.action, "would_remove");
    assert_eq!(report.run_id.as_deref(), Some("jrun-20260726-0305-2"));
    assert_eq!(report.run_state, Some(JobRunState::Success));
    assert_eq!(report.task_id.as_deref(), Some("ORB-10419"));
    assert_eq!(report.task_status, Some(TaskStatus::Done));
    assert_eq!(
        report.bytes_reclaimed, 0,
        "dry-run skips the directory walk unless estimate_bytes is set"
    );
    assert!(result.dry_run);
    assert!(worktree.exists(), "a dry run never removes anything");
}

#[test]
fn dry_run_estimates_bytes_only_when_requested() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-estimate", JobRunState::Success, &["ORB-ESTIMATE"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/estimate");
    fs::write(worktree.join("bytes.bin"), [1_u8; 32]).unwrap();
    git(&worktree, &["add", "bytes.bin"]);
    git(&worktree, &["commit", "-m", "worktree content"]);
    let host = FakeTaskHost::new(vec![task_fixture("ORB-ESTIMATE", TaskStatus::Done)]);

    let estimated = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            estimate_bytes: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(estimated.reports[0].action, "would_remove");
    assert!(
        estimated.reports[0].bytes_reclaimed > 0,
        "estimate_bytes restores the dry-run directory walk"
    );
    assert!(estimated.dry_run);
    assert!(worktree.exists(), "a dry run never removes anything");
}

/// The bug was a silent divergence between two independent spellings of one
/// rule: `setup_worktree` created `orbit-<run_id>` and gc looked for
/// `parallel-batch-<run_id>`. Both now derive through [`WorktreeIdentity`].
#[test]
fn setup_and_gc_derive_the_same_worktree_path() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let run = pipeline_run("jrun-derivation", JobRunState::Success, &["ORB-DERIVE"]);
    let input = run.input.clone().unwrap();

    let identity = WorktreeIdentity::from_input(&input, Some(&run.run_id)).unwrap();

    assert_eq!(identity.task_ids, vec!["ORB-DERIVE".to_string()]);
    assert_eq!(identity.branch_prefix, "orbit");
    assert_eq!(identity.run_id, "jrun-derivation");
    assert_eq!(
        identity.path(&repo).unwrap(),
        resolved_task_worktree(&repo, &run)
    );
}

/// [ORB-12491] `epic_pipeline` is retired, but its stored runs are not: GC
/// re-derives worktree identity from run input, so the historical decoding must
/// keep resolving to the directory that run's `worktree_setup` created.
#[test]
fn stored_epic_pipeline_input_matches_worktree_setup_path() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let run = epic_pipeline_run("jrun-epic-derivation", JobRunState::Success, "ORB-EPIC");
    let stored_input = run.input.clone().unwrap();

    let gc_identity = WorktreeIdentity::from_input(&stored_input, Some(&run.run_id)).unwrap();
    let setup_input = json!({
        "task_ids": ["ORB-EPIC"],
        "run_id": "epic-ORB-EPIC",
        "branch_prefix": "epic",
    });
    let setup_identity = WorktreeIdentity::from_input(&setup_input, None).unwrap();

    assert_eq!(gc_identity.task_ids, vec!["ORB-EPIC".to_string()]);
    assert_eq!(gc_identity.branch_prefix, "epic");
    assert_eq!(gc_identity.run_id, "epic-ORB-EPIC");
    assert_eq!(
        gc_identity.path(&repo).unwrap(),
        setup_identity.path(&repo).unwrap()
    );
}

/// [ORB-12491] The sweep must still reap what a retired epic run left behind.
#[test]
fn epic_pipeline_worktree_is_collected_from_stored_run_input() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = epic_pipeline_run("jrun-epic-gc", JobRunState::Success, "ORB-EPIC-GC");
    let worktree = resolve_worktree_path_from_prefix(&repo, "epic", "epic-ORB-EPIC-GC").unwrap();
    add_worktree(&repo, &worktree, "epic/ORB-EPIC-GC");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-EPIC-GC", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        std::slice::from_ref(&run),
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let report = result
        .reports
        .iter()
        .find(|report| report.path == worktree)
        .expect("the epic worktree must be classified from the stored run input");
    assert_eq!(report.action, "removed");
    assert_eq!(report.run_id.as_deref(), Some("jrun-epic-gc"));
    assert_eq!(report.task_id.as_deref(), Some("ORB-EPIC-GC"));
    assert_eq!(report.task_status, Some(TaskStatus::Done));
    assert!(report.bytes_reclaimed > 0);
    assert!(!worktree.exists());
}

/// Stored runs from before `task_ids` existed still carry the singular key.
#[test]
fn legacy_singular_task_id_run_is_still_recognized() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = legacy_task_id_run("jrun-legacy", JobRunState::Success, "ORB-LEGACY");
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/legacy");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-LEGACY", TaskStatus::Done)]);

    let result = collect_worktrees(&repo, &[run], &host, &WorktreeGcOptions::default()).unwrap();

    assert_eq!(result.reports[0].action, "would_remove");
    assert_eq!(result.reports[0].task_id.as_deref(), Some("ORB-LEGACY"));
}

/// The second divergence of the same shape: `setup_worktree` falls back to a
/// task-derived token when no `run_id` reaches it, while gc only ever knew the
/// run record's id. gc now considers both candidates.
#[test]
fn worktree_created_under_the_task_derived_fallback_is_recognized() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-fallback", JobRunState::Success, &["ORB-FALLBACK"]);
    let fallback = resolve_worktree_path_from_prefix(&repo, "orbit", "task-ORB-FALLBACK").unwrap();
    assert_ne!(fallback, resolved_task_worktree(&repo, &run));
    add_worktree(&repo, &fallback, "orbit/fallback");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-FALLBACK", TaskStatus::Blocked)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let report = result
        .reports
        .iter()
        .find(|report| report.path == fallback)
        .expect("the fallback-named worktree must be attributed to its run");
    assert_eq!(report.action, "skipped:task_status_ineligible");
    assert_eq!(report.run_id.as_deref(), Some("jrun-fallback"));
    assert!(fallback.exists());
}

/// Bundle rule: a worktree serving several tasks is only eligible when every
/// task it serves is settled, and the report names the member that blocked it.
/// A bundle is never easier to discard than its least-settled member.
#[test]
fn bundle_worktree_is_retained_until_every_member_settles() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run(
        "jrun-bundle",
        JobRunState::Success,
        &["ORB-BUNDLE-A", "ORB-BUNDLE-B"],
    );
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/bundle-0badc0de");
    let host = FakeTaskHost::new(vec![
        task_fixture("ORB-BUNDLE-A", TaskStatus::Done),
        task_fixture("ORB-BUNDLE-B", TaskStatus::Review),
    ]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:task_status_ineligible");
    assert_eq!(
        result.reports[0].task_id.as_deref(),
        Some("ORB-BUNDLE-B"),
        "the report names the member that blocked the bundle"
    );
    assert_eq!(result.reports[0].task_status, Some(TaskStatus::Review));
}

#[test]
fn bundle_worktree_with_every_member_settled_is_eligible() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run(
        "jrun-bundle-settled",
        JobRunState::Success,
        &["ORB-BUNDLE-A", "ORB-BUNDLE-B"],
    );
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/bundle-5ettled");
    let host = FakeTaskHost::new(vec![
        task_fixture("ORB-BUNDLE-A", TaskStatus::Done),
        task_fixture("ORB-BUNDLE-B", TaskStatus::Archived),
    ]);

    let result = collect_worktrees(&repo, &[run], &host, &WorktreeGcOptions::default()).unwrap();

    assert_eq!(result.reports[0].action, "would_remove");
    assert_eq!(
        result.reports[0].task_id.as_deref(),
        Some("ORB-BUNDLE-A,ORB-BUNDLE-B"),
        "an eligible bundle names every task it serves"
    );
}

/// Safety gate: a worktree may still back a live process.
#[test]
fn non_terminal_run_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-running", JobRunState::Running, &["ORB-RUNNING"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/running");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-RUNNING", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:run_not_terminal");
}

/// Safety gate: `--older-than-hours` holds back recently finished runs.
#[test]
fn run_finished_after_the_cutoff_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-recent", JobRunState::Success, &["ORB-RECENT"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/recent");
    let host = FakeTaskHost::new(vec![task_fixture("ORB-RECENT", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            older_than: Some(Utc::now() - Duration::hours(1)),
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:too_recent");
}

/// Safety gate: the collector never follows a symlink standing where a
/// worktree should be.
#[cfg(unix)]
#[test]
fn symlink_at_a_known_worktree_path_is_never_followed() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-symlink", JobRunState::Success, &["ORB-SYMLINK"]);
    let worktree = resolved_task_worktree(&repo, &run);
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("precious.txt"), "keep me").unwrap();
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &worktree).unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-SYMLINK", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(elsewhere.join("precious.txt").exists());
    assert_eq!(result.reports[0].action, "skipped:not_a_real_directory");
}

/// Safety gate: a directory Git does not know as a worktree is left alone,
/// even at a path a run record claims.
#[test]
fn unregistered_directory_at_a_known_path_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-unregistered", JobRunState::Success, &["ORB-UNREG"]);
    let worktree = resolved_task_worktree(&repo, &run);
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("rescue.txt"), "keep me").unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-UNREG", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.join("rescue.txt").exists());
    assert_eq!(result.reports[0].action, "skipped:not_registered_worktree");
}

/// Safety gate: a task the store cannot resolve says nothing about whether
/// the work settled, so the worktree is retained.
#[test]
fn run_whose_task_cannot_be_resolved_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-missing", JobRunState::Success, &["ORB-MISSING"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/missing");
    let host = FakeTaskHost::new(Vec::new());

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:task_unresolved");
    assert_eq!(result.reports[0].task_id.as_deref(), Some("ORB-MISSING"));
    assert_eq!(result.reports[0].task_status, None);
}

/// Safety gate: a detached worktree has no branch to reason about, so it is
/// never collected.
#[test]
fn detached_worktree_with_no_branch_is_retained() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-detached", JobRunState::Success, &["ORB-DETACHED"]);
    let worktree = resolved_task_worktree(&repo, &run);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let host = FakeTaskHost::new(vec![task_fixture("ORB-DETACHED", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(worktree.exists());
    assert_eq!(result.reports[0].action, "skipped:branch_unknown");
}

/// The removal itself is the last gate: gc calls `remove_worktree` without
/// `--force`, so a worktree dirtied after the status check makes Git refuse.
/// Never replace this with a recursive delete.
#[test]
fn removal_without_force_fails_closed_on_a_dirty_worktree() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-noforce");
    add_worktree(&repo, &worktree, "orbit/noforce");
    fs::write(worktree.join("uncommitted.txt"), "valuable").unwrap();

    let error = remove_worktree(&repo, &worktree, Some("orbit/noforce"), false)
        .expect_err("git must refuse to remove a dirty worktree without --force");

    assert!(worktree.join("uncommitted.txt").exists());
    assert!(
        format!("{error}").contains("worktree remove"),
        "unexpected error: {error}"
    );
}

/// [DANI-10448] A worktree carrying a large synthetic `target/`-shaped tree
/// timed out `git worktree remove` in production (a multi-GB, millions-of-file
/// directory cannot be unlinked inside the 30s git budget on APFS), and that
/// timeout aborted the whole sweep before it reached any other worktree. GC
/// must instead reclaim it well inside the sweep's budget by relocating the
/// tree out of the way and pruning Git's metadata immediately, finishing the
/// bulk delete off the critical path.
#[test]
fn large_synthetic_worktree_is_reclaimed_within_the_gc_budget() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-m", "ignore build output"]);
    let run = pipeline_run("jrun-large", JobRunState::Success, &["ORB-LARGE"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/large");
    let build_dir = worktree.join("target");
    fs::create_dir_all(&build_dir).unwrap();
    for index in 0..4_000 {
        fs::write(
            build_dir.join(format!("artifact-{index}.o")),
            b"fixture build output",
        )
        .unwrap();
    }
    let host = FakeTaskHost::new(vec![task_fixture("ORB-LARGE", TaskStatus::Done)]);

    let started = std::time::Instant::now();
    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "GC took {elapsed:?}, longer than the sweep budget allows"
    );
    assert_eq!(result.reports[0].action, "removed");
    assert!(result.reports[0].bytes_reclaimed > 0);
    assert!(
        !worktree.exists(),
        "the worktree path is relocated out of the way immediately"
    );
    assert!(
        !is_registered_worktree(&repo, &worktree).unwrap(),
        "Git metadata must be pruned even while the bulk delete is still running"
    );

    // The background deletion eventually finishes too, so no `.trash-*`
    // leftover survives indefinitely.
    wait_for_trash_cleanup(&worktree);
}

/// A supervisor timeout can land before `git worktree remove` has finished
/// its clean and lock checks. A budget of [`GitTimeoutBudget::MIN_MS`]
/// reliably forces exactly that — spawning `git` alone takes longer than 1ms,
/// so Git never reaches its checks — and the recovery's own verification
/// cannot finish inside that budget either. The timeout alone must not
/// authorize deletion: the dirty bytes and the registration stay put, now and
/// after any background work would have had time to run.
#[test]
fn removal_timeout_before_safety_checks_preserves_a_dirty_worktree() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-timeout");
    add_worktree(&repo, &worktree, "orbit/timeout");
    fs::write(worktree.join("marker.txt"), "still here").unwrap();

    let error = {
        let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
            default_ms: GitTimeoutBudget::MIN_MS,
            ..GitTimeoutBudget::DEFAULT
        });
        remove_worktree(&repo, &worktree, None, false)
            .expect_err("an unverified git worktree remove timeout must not count as removal")
    };

    assert!(
        format!("{error}").contains("preserved worktree"),
        "the error must say the worktree was preserved: {error}"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(
        fs::read_to_string(worktree.join("marker.txt")).unwrap(),
        "still here"
    );
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
    assert_eq!(
        trash_siblings(&worktree),
        0,
        "nothing was relocated for deletion"
    );
}

/// Even a clean tree is not deleted on the strength of a timeout: when the
/// independent verification cannot complete, the worktree stays registered.
#[test]
fn removal_timeout_never_deletes_an_unverified_clean_worktree() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-unverified");
    add_worktree(&repo, &worktree, "orbit/unverified");

    {
        let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
            default_ms: GitTimeoutBudget::MIN_MS,
            ..GitTimeoutBudget::DEFAULT
        });
        remove_worktree_without_force(&repo, &worktree)
            .expect_err("a timeout whose recovery cannot verify the tree must fail closed");
    }

    assert!(worktree.join("base.txt").exists());
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
    assert_eq!(trash_siblings(&worktree), 0);
}

/// GC's status scan runs before removal, so content can appear in between.
/// When the non-force removal then times out, the recovery's own checks —
/// run at the normal budget — see that content and preserve every byte.
#[test]
fn worktree_dirtied_after_the_scan_survives_an_interrupted_removal() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-late-dirt");
    add_worktree(&repo, &worktree, "orbit/late-dirt");
    fs::write(worktree.join("base.txt"), "edited after the scan").unwrap();
    fs::create_dir_all(worktree.join("notes")).unwrap();
    fs::write(worktree.join("notes/untracked.md"), "new work").unwrap();

    let error = recover_timed_out_removal(&worktree, 30_000)
        .expect_err("a dirty tree must never be relocated for deletion");

    assert!(
        format!("{error}").contains("uncommitted or untracked content"),
        "unexpected error: {error}"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("base.txt")).unwrap(),
        "edited after the scan"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("notes/untracked.md")).unwrap(),
        "new work"
    );
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
    assert_eq!(trash_siblings(&worktree), 0);
}

/// Git refuses to remove a locked worktree before looking at its contents;
/// an interrupted removal must honor the same refusal.
#[test]
fn locked_worktree_survives_an_interrupted_removal() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-locked");
    add_worktree(&repo, &worktree, "orbit/locked");
    git(&repo, &["worktree", "lock", worktree.to_str().unwrap()]);

    let error = recover_timed_out_removal(&worktree, 30_000)
        .expect_err("a locked tree must never be relocated for deletion");

    assert!(
        format!("{error}").contains("locked"),
        "unexpected error: {error}"
    );
    assert!(worktree.join("base.txt").exists());
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
}

/// Git refuses to remove a worktree holding a populated submodule, whose
/// history may exist nowhere else, even when the superproject is clean.
#[test]
fn worktree_with_a_populated_gitlink_survives_an_interrupted_removal() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-gitlink");
    add_worktree(&repo, &worktree, "orbit/gitlink");
    let nested = worktree.join("nested");
    init_repo(&nested);
    git(&worktree, &["add", "nested"]);
    git(&worktree, &["commit", "-m", "embed nested repository"]);

    let error = recover_timed_out_removal(&worktree, 30_000)
        .expect_err("a tree holding submodule history must never be relocated for deletion");

    assert!(
        format!("{error}").contains("submodules"),
        "unexpected error: {error}"
    );
    assert!(nested.join("base.txt").exists());
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
}

/// [DANI-10448] The timeout this recovery exists for: Git approved a clean
/// tree and was killed partway through physically deleting a huge ignored
/// `target/`, having already removed some tracked files. Verification sees
/// only missing tracked files — no bytes to lose — so the tree is relocated
/// at once, Git's metadata is prunable immediately, and the bulk delete
/// finishes off the critical path.
#[test]
fn interrupted_physical_delete_of_a_clean_worktree_is_finished_in_the_background() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-m", "ignore build output"]);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-interrupted");
    add_worktree(&repo, &worktree, "orbit/interrupted");
    let build_dir = worktree.join("target");
    fs::create_dir_all(&build_dir).unwrap();
    for index in 0..4_000 {
        fs::write(
            build_dir.join(format!("artifact-{index}.o")),
            b"fixture build output",
        )
        .unwrap();
    }
    // What Git's own delete had already unlinked when the deadline hit.
    fs::remove_file(worktree.join("base.txt")).unwrap();

    let started = std::time::Instant::now();
    recover_timed_out_removal(&worktree, 30_000)
        .expect("a verified-clean, partially deleted tree must be recovered");
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "recovery took {elapsed:?}; the bulk delete must stay off the critical path"
    );
    assert!(
        !worktree.exists(),
        "the worktree is relocated to a trash sibling, not left in place"
    );
    git(&repo, &["worktree", "prune"]);
    assert!(
        !is_registered_worktree(&repo, &worktree).unwrap(),
        "Git metadata must be prunable immediately, without waiting on the relocated tree's deletion"
    );
    wait_for_trash_cleanup(&worktree);
}

/// [DANI-10448] One candidate's hard failure — here, a linked worktree whose
/// `.git` pointer file is corrupted, so any git command run inside it fails
/// outright — must not stop the sweep from reaching every other candidate.
/// `collect_worktrees` reports the failure and keeps going; the pass as a
/// whole still succeeds with a partial summary.
#[test]
fn one_failing_worktree_does_not_abort_the_rest_of_the_sweep() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);

    let broken_run = pipeline_run("jrun-broken", JobRunState::Success, &["ORB-BROKEN"]);
    let broken_worktree = resolved_task_worktree(&repo, &broken_run);
    add_worktree(&repo, &broken_worktree, "orbit/broken");
    fs::write(
        broken_worktree.join(".git"),
        "gitdir: /nonexistent/orbit-test-gitdir",
    )
    .unwrap();

    let healthy_run = pipeline_run("jrun-healthy", JobRunState::Success, &["ORB-HEALTHY"]);
    let healthy_worktree = resolved_task_worktree(&repo, &healthy_run);
    add_worktree(&repo, &healthy_worktree, "orbit/healthy");

    let host = FakeTaskHost::new(vec![
        task_fixture("ORB-BROKEN", TaskStatus::Done),
        task_fixture("ORB-HEALTHY", TaskStatus::Done),
    ]);

    let result = collect_worktrees(
        &repo,
        &[broken_run, healthy_run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .expect("one candidate's failure must not fail the whole sweep");

    let broken_report = result
        .reports
        .iter()
        .find(|report| report.path == broken_worktree)
        .expect("the broken worktree must still be reported");
    assert!(
        broken_report.action.starts_with("failed:"),
        "expected a failed outcome, got {}",
        broken_report.action
    );
    assert!(
        broken_worktree.exists(),
        "a failed candidate must be left in place, not partially touched"
    );

    let healthy_report = result
        .reports
        .iter()
        .find(|report| report.path == healthy_worktree)
        .expect("the sweep must still reach the healthy candidate");
    assert_eq!(healthy_report.action, "removed");
    assert!(!healthy_worktree.exists());
}

/// [ORB-13658] A follower replica has no local task records, so every run
/// used to be `skipped:task_unresolved` and nothing was ever reaped. The
/// settled rule is applied to the owner's answer instead.
#[test]
fn replica_worktree_is_reaped_when_its_owner_reports_the_task_done() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let settled = pipeline_run("jrun-replica-done", JobRunState::Success, &["ORB-OWNED"]);
    let settled_worktree = resolved_task_worktree(&repo, &settled);
    add_worktree(&repo, &settled_worktree, "orbit/replica-done");
    let live = pipeline_run("jrun-replica-live", JobRunState::Failed, &["ORB-LIVE"]);
    let live_worktree = resolved_task_worktree(&repo, &live);
    add_worktree(&repo, &live_worktree, "orbit/replica-live");
    let host = ReplicaHost::owner_answers(&[
        ("ORB-OWNED", TaskStatus::Done),
        ("ORB-LIVE", TaskStatus::InProgress),
    ]);

    let result = collect_worktrees(
        &repo,
        &[settled, live],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    let settled_report = report_for(&result.reports, &settled_worktree);
    assert_eq!(settled_report.action, "removed");
    assert_eq!(settled_report.task_status, Some(TaskStatus::Done));
    assert!(!settled_worktree.exists());
    let live_report = report_for(&result.reports, &live_worktree);
    assert_eq!(live_report.action, "skipped:task_status_ineligible");
    assert_eq!(live_report.task_status, Some(TaskStatus::InProgress));
    assert!(live_worktree.exists());
}

/// An owner that cannot be reached says nothing about whether the task
/// exists: the report names the outage rather than `task_unresolved`, and the
/// sweep stops waiting on the owner after the first failure.
#[test]
fn replica_worktree_is_retained_as_owner_unreachable_when_the_owner_cannot_answer() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let first = pipeline_run("jrun-offline-a", JobRunState::Success, &["ORB-OFFLINE-A"]);
    let first_worktree = resolved_task_worktree(&repo, &first);
    add_worktree(&repo, &first_worktree, "orbit/offline-a");
    let second = pipeline_run("jrun-offline-b", JobRunState::Success, &["ORB-OFFLINE-B"]);
    let second_worktree = resolved_task_worktree(&repo, &second);
    add_worktree(&repo, &second_worktree, "orbit/offline-b");
    let host = ReplicaHost::owner_unreachable();

    let result = collect_worktrees(
        &repo,
        &[first, second],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    for worktree in [&first_worktree, &second_worktree] {
        let report = report_for(&result.reports, worktree);
        assert_eq!(report.action, "skipped:owner_unreachable");
        assert_eq!(report.task_status, None);
        assert!(worktree.exists());
    }
    assert_eq!(
        host.owner_calls.load(Ordering::SeqCst),
        1,
        "one unreachable answer ends the sweep's owner lookups"
    );
}

/// [ORB-13658] Target-only mode reclaims the Cargo build output of a failed
/// run whose task is still open on an unreachable owner, and keeps the
/// checkout, its uncommitted work and its branch for rescue.
#[test]
fn target_only_reclaims_build_output_and_keeps_the_checkout() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    ignore_build_output(&repo);
    let run = pipeline_run("jrun-target", JobRunState::Failed, &["ORB-TARGET"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/target");
    fs::write(worktree.join("uncommitted.txt"), "rescue me").unwrap();
    write_build_output(&worktree);
    let host = ReplicaHost::owner_unreachable();

    let dry = collect_worktrees(
        &repo,
        std::slice::from_ref(&run),
        &host,
        &WorktreeGcOptions {
            target_only: true,
            estimate_bytes: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(dry.reports[0].action, "would_remove_target");
    assert!(dry.dry_run);
    assert!(dry.reports[0].bytes_reclaimed > 0);
    assert!(
        worktree.join("target").exists(),
        "a dry run removes nothing"
    );

    let applied = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            target_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    let report = &applied.reports[0];
    assert_eq!(report.action, "removed_target");
    assert_eq!(report.bytes_reclaimed, dry.reports[0].bytes_reclaimed);
    assert_eq!(applied.bytes_reclaimed, report.bytes_reclaimed);
    assert!(!worktree.join("target").exists());
    assert_eq!(
        fs::read_to_string(worktree.join("uncommitted.txt")).unwrap(),
        "rescue me"
    );
    assert!(is_registered_worktree(&repo, &worktree).unwrap());
    assert!(git(&repo, &["branch", "--list", "orbit/target"]).contains("orbit/target"));
    assert_eq!(
        host.owner_calls.load(Ordering::SeqCst),
        0,
        "target-only collection does not depend on task state"
    );
}

/// Safety gate: a running run's build output is in use.
#[test]
fn target_only_never_touches_a_running_run() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    ignore_build_output(&repo);
    let run = pipeline_run("jrun-target-running", JobRunState::Running, &["ORB-RUN"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/target-running");
    write_build_output(&worktree);
    let host = FakeTaskHost::new(vec![task_fixture("ORB-RUN", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            target_only: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.reports[0].action, "skipped:run_not_terminal");
    assert_eq!(result.reports[0].bytes_reclaimed, 0);
    assert!(worktree.join("target/artifact-0.o").exists());
}

/// Safety gate: a terminal run record can precede its worker's exit, so a
/// recorded worker that is still alive keeps its build output.
#[test]
fn target_only_keeps_build_output_while_the_recorded_worker_lives() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    ignore_build_output(&repo);
    let mut run = pipeline_run("jrun-target-worker", JobRunState::Cancelled, &["ORB-W"]);
    run.pid = Some(std::process::id());
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/target-worker");
    write_build_output(&worktree);
    let host = FakeTaskHost::new(Vec::new());

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            target_only: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.reports[0].action, "skipped:worker_alive");
    assert!(worktree.join("target/artifact-0.o").exists());
}

/// Safety gate: content under `target/` that Git tracks, or does not
/// ignore, is somebody's work rather than build output.
#[test]
fn target_only_keeps_a_target_directory_git_does_not_ignore() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-target-kept", JobRunState::Success, &["ORB-KEPT"]);
    let worktree = resolved_task_worktree(&repo, &run);
    add_worktree(&repo, &worktree, "orbit/target-kept");
    write_build_output(&worktree);
    let host = FakeTaskHost::new(Vec::new());

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            target_only: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.reports[0].action, "skipped:target_not_ignored");
    assert!(worktree.join("target/artifact-0.o").exists());
}

fn report_for<'a>(reports: &'a [WorktreeGcReport], worktree: &Path) -> &'a WorktreeGcReport {
    reports
        .iter()
        .find(|report| report.path == worktree)
        .expect("every known worktree is reported")
}

/// Commit the `/target` ignore rule Cargo workspaces carry, before any
/// worktree branches off.
fn ignore_build_output(repo: &Path) {
    fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    git(repo, &["add", ".gitignore"]);
    git(repo, &["commit", "-m", "ignore build output"]);
}

fn write_build_output(worktree: &Path) {
    let build_dir = worktree.join("target/debug");
    fs::create_dir_all(&build_dir).unwrap();
    fs::write(worktree.join("target/artifact-0.o"), [7_u8; 64]).unwrap();
    fs::write(build_dir.join("orbit"), [9_u8; 128]).unwrap();
}

/// A run record shaped exactly like a real `task_pr_pipeline` run: `task_ids`
/// as an array, no `branch_prefix`, and no singular `task_id`. Also no
/// `run_id` — the engine injects that into the activity input at dispatch, so
/// the stored `initial_input` never carries it.
///
/// Copied from run `jrun-20260726-0305-2` on dk-server-1 (ORB-10427). GC
/// probed the singular `task_id` against this shape, missed, derived a
/// `parallel-batch-*` path that no worktree ever occupied, and so classified
/// every real worktree `skipped:unrecognized`.
fn pipeline_run(id: &str, state: JobRunState, task_ids: &[&str]) -> JobRun {
    job_run(
        id,
        state,
        json!({
            "auto_push": true,
            "base_branch": "agent-main",
            "base_sync": "remote",
            "review": false,
            "task_ids": task_ids,
        }),
    )
}

/// A pre-`task_ids` run record. Stored runs from older pipelines still carry
/// the singular key, and gc must keep recognizing them.
fn legacy_task_id_run(id: &str, state: JobRunState, task_id: &str) -> JobRun {
    job_run(id, state, json!({ "task_id": task_id }))
}

fn epic_pipeline_run(id: &str, state: JobRunState, epic_task_id: &str) -> JobRun {
    let mut run = job_run(id, state, json!({ "epic_task_id": epic_task_id }));
    run.job_id = "epic_pipeline".to_string();
    run
}

/// A run that names no task at all — it never went through `setup_worktree`.
fn unattributed_run(id: &str, state: JobRunState) -> JobRun {
    job_run(id, state, json!({}))
}

fn job_run(id: &str, state: JobRunState, input: Value) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: id.to_string(),
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

/// The directory `setup_worktree` creates for a run with no `branch_prefix`
/// override, spelled out independently of the production derivation so these
/// tests pin the on-disk outcome rather than restating the code under test.
/// [`setup_and_gc_derive_the_same_worktree_path`] ties the two together.
fn resolved_task_worktree(repo: &Path, run: &JobRun) -> PathBuf {
    resolve_worktree_path_from_prefix(repo, "orbit", &run.run_id).unwrap()
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", "agent-main"]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(path.join("base.txt"), "base").unwrap();
    git(path, &["add", "base.txt"]);
    git(path, &["commit", "-m", "base"]);
}

fn add_worktree(repo: &Path, path: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
}

/// `.trash-*` siblings a relocated worktree leaves until its background
/// deletion finishes.
fn trash_siblings(worktree: &Path) -> usize {
    fs::read_dir(worktree.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".trash-")
        })
        .count()
}

fn wait_for_trash_cleanup(worktree: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while trash_siblings(worktree) > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "background trash deletion did not finish in time"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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

fn is_resolver_env_child(case: &str) -> bool {
    std::env::var_os(RESOLVER_ENV_CHILD).is_some_and(|value| value == case)
}

fn run_resolver_test_in_child(test_name: &str, case: &str, root: Option<&Path>) {
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap_or(module_path!());
    let exact_test_name = format!("{module}::{test_name}");
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &exact_test_name, "--nocapture"])
        .env(RESOLVER_ENV_CHILD, case);
    match root {
        Some(root) => {
            command.env("ORBIT_WORKTREE_ROOT", root);
        }
        None => {
            command.env_remove("ORBIT_WORKTREE_ROOT");
        }
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "isolated resolver test {exact_test_name} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
