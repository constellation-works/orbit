#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{Duration, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::context::RuntimeHost;

use super::super::cleanup::remove_worktree;
use super::super::gc::{WorktreeGcOptions, collect_worktrees};
use super::super::{
    WorktreeIdentity, resolve_shared_worktree_path, resolve_worktree_path_from_prefix,
};

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

fn task_fixture(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
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
    assert!(
        report.bytes_reclaimed > 0,
        "a dry run still estimates what --yes would reclaim"
    );
    assert!(result.dry_run);
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
