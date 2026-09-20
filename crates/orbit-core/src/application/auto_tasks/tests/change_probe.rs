//! `skip_if_unchanged` evidence tests [ORB-12698]: cursor-task selection
//! (including the legacy-tag fallback), cursor-artifact reading, and the
//! fail-open answer when git cannot resolve the configured ref.

use std::path::Path;
use std::process::Command;

use orbit_automation::auto_tasks::scheduler::ChangeProbe;
use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::workflow::{SWEEP_CURSOR_ARTIFACT, SkipIfUnchanged, SweepCursorSelector};
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::auto_tasks::change_probe::{
    newest_completed_sweep, probe_change_since_last_sweep, read_cursor, select_sweep,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn runtime() -> OrbitRuntime {
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

fn precondition(tags: &[&str], legacy_tags: &[&str]) -> SkipIfUnchanged {
    SkipIfUnchanged {
        reference: "agent-main".to_string(),
        cursor: SweepCursorSelector {
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            legacy_tags: legacy_tags.iter().map(|tag| (*tag).to_string()).collect(),
        },
    }
}

/// A candidate with only the fields selection reads.
fn candidate(id: &str, created_at: &str, status: &str, task_type: &str) -> Task {
    serde_json::from_value(json!({
        "id": id,
        "title": "Review recently merged changes",
        "description": "",
        "context_files": [],
        "status": status,
        "priority": "medium",
        "task_type": task_type,
        "created_at": created_at,
        "updated_at": created_at,
    }))
    .expect("task fixture")
}

/// One completed sweep chore carrying `tags`, with an optional cursor artifact.
fn completed_sweep(runtime: &OrbitRuntime, tags: &[&str], cursor: Option<&str>) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review recently merged changes".to_string(),
            tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
            task_type: Some(orbit_types::task::TaskType::Chore),
            ..TaskAddParams::default()
        })
        .expect("add sweep");
    if let Some(content) = cursor {
        runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    upsert_artifacts: vec![TaskArtifact::from_text(
                        SWEEP_CURSOR_ARTIFACT,
                        content.to_string(),
                    )],
                    ..Default::default()
                },
            )
            .expect("attach cursor artifact");
    }
    runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("complete sweep");
    task.id.to_string()
}

#[test]
fn selection_takes_the_newest_completed_chore_and_breaks_ties_on_id() {
    let selected = select_sweep(vec![
        candidate("ORB-12690", "2026-09-20T10:00:00Z", "done", "chore"),
        // Same instant: the lexicographically smaller id wins.
        candidate("ORB-12697", "2026-09-20T11:00:00Z", "done", "chore"),
        candidate("ORB-12696", "2026-09-20T11:00:00Z", "done", "chore"),
    ]);
    assert_eq!(
        selected.map(|task| task.id.to_string()),
        Some("ORB-12696".to_string())
    );
}

#[test]
fn selection_ignores_open_work_and_findings_sharing_the_sweep_tags() {
    // A `bug` finding filed by a sweep carries the same tags and may complete
    // later than the sweep itself; it never records a cursor.
    let selected = select_sweep(vec![
        candidate("ORB-12704", "2026-09-20T12:00:00Z", "done", "bug"),
        candidate("ORB-12703", "2026-09-20T11:30:00Z", "in-progress", "chore"),
        candidate("ORB-12696", "2026-09-20T10:00:00Z", "done", "chore"),
    ]);
    assert_eq!(
        selected.map(|task| task.id.to_string()),
        Some("ORB-12696".to_string())
    );
}

#[test]
fn legacy_tags_are_consulted_only_when_the_current_tags_select_nothing() {
    let runtime = runtime();
    let legacy = completed_sweep(&runtime, &["code-review-sweep", "no-diff-expected"], None);
    let precondition = precondition(
        &["code-review", "no-diff-expected"],
        &["code-review-sweep", "no-diff-expected"],
    );

    assert_eq!(
        newest_completed_sweep(&runtime, &precondition)
            .expect("selection")
            .map(|task| task.id.to_string()),
        Some(legacy)
    );

    // A sweep under the current name supersedes the legacy record.
    let current = completed_sweep(&runtime, &["code-review", "no-diff-expected"], None);
    assert_eq!(
        newest_completed_sweep(&runtime, &precondition)
            .expect("selection")
            .map(|task| task.id.to_string()),
        Some(current)
    );
}

#[test]
fn no_completed_sweep_selects_nothing() {
    let runtime = runtime();
    assert!(
        newest_completed_sweep(&runtime, &precondition(&["code-review"], &[]))
            .expect("selection")
            .is_none()
    );
}

#[test]
fn cursor_artifact_is_read_structurally_and_unusable_records_are_rejected() {
    let runtime = runtime();

    let without = completed_sweep(&runtime, &["code-review"], None);
    assert_eq!(read_cursor(&runtime, &without), Ok(None));

    let good = completed_sweep(
        &runtime,
        &["code-review"],
        Some(
            r#"{"schema_version":1,"ref":"agent-main","cursor":"58779d949b233cce10af1bf38df1f9cb11476fa0","reviewed_range":"58779d94..58779d94"}"#,
        ),
    );
    let record = read_cursor(&runtime, &good)
        .expect("readable")
        .expect("present");
    assert_eq!(record.reference, "agent-main");
    assert_eq!(record.cursor, "58779d949b233cce10af1bf38df1f9cb11476fa0");

    let prose = completed_sweep(
        &runtime,
        &["code-review"],
        Some("New last-reviewed commit: 58779d94"),
    );
    assert!(
        read_cursor(&runtime, &prose)
            .expect_err("prose is not a cursor")
            .contains("is not a cursor record")
    );

    let future = completed_sweep(
        &runtime,
        &["code-review"],
        Some(r#"{"schema_version":2,"ref":"agent-main","cursor":"58779d94"}"#),
    );
    assert!(
        read_cursor(&runtime, &future)
            .expect_err("unknown schema version")
            .contains("schema_version 2")
    );
}

#[test]
fn an_unresolvable_ref_is_inconclusive_rather_than_an_error() {
    let runtime = runtime();
    completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected"],
        Some(
            r#"{"schema_version":1,"ref":"agent-main","cursor":"58779d949b233cce10af1bf38df1f9cb11476fa0"}"#,
        ),
    );

    // The in-memory runtime's repo root is not a git checkout, so git cannot
    // answer — the scheduler must still mint.
    let probe = probe_change_since_last_sweep(
        &runtime,
        &precondition(&["code-review", "no-diff-expected"], &[]),
    )
    .expect("probe");
    match probe {
        ChangeProbe::Unknown { reason } => assert!(reason.contains("agent-main"), "{reason}"),
        other => panic!("expected an inconclusive probe, got {other:?}"),
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout)
        .expect("git output")
        .trim()
        .into()
}

/// A workspace whose checkout is a real repository on `agent-main`, so the
/// probe exercises git rather than a stand-in.
fn git_workspace() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("temporary root");
    let global_root = root.path().join("global");
    std::fs::create_dir_all(&global_root).expect("global root");
    let repo_root = root.path().join("checkout");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    git(&repo_root, &["init", "--initial-branch=agent-main"]);
    git(&repo_root, &["config", "user.name", "Test"]);
    git(
        &repo_root,
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(repo_root.join("sample.txt"), "baseline").expect("seed file");
    git(&repo_root, &["add", "sample.txt"]);
    git(&repo_root, &["commit", "-m", "baseline"]);
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    (root, runtime)
}

fn commit(repo_root: &Path, content: &str) -> String {
    std::fs::write(repo_root.join("sample.txt"), content).expect("write file");
    git(repo_root, &["add", "sample.txt"]);
    git(repo_root, &["commit", "-m", content]);
    git(repo_root, &["rev-parse", "HEAD"])
}

#[test]
fn a_quiet_branch_is_unchanged_and_one_new_commit_is_changed() {
    let (root, runtime) = git_workspace();
    let repo_root = root.path().join("checkout");
    let baseline = git(&repo_root, &["rev-parse", "HEAD"]);
    let precondition = precondition(&["code-review", "no-diff-expected"], &[]);
    completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected"],
        Some(&format!(
            r#"{{"schema_version":1,"ref":"agent-main","cursor":"{baseline}"}}"#
        )),
    );

    assert!(
        matches!(
            probe_change_since_last_sweep(&runtime, &precondition).expect("probe"),
            ChangeProbe::Unchanged { ref cursor, ref tip, .. } if cursor == &baseline && tip == &baseline
        ),
        "a branch that has not moved is covered by its own cursor"
    );

    let landed = commit(&repo_root, "one landed change");
    match probe_change_since_last_sweep(&runtime, &precondition).expect("probe") {
        ChangeProbe::Changed { cursor, tip } => {
            assert_eq!(cursor, baseline);
            assert_eq!(tip, landed);
        }
        other => panic!("one new commit must mint, got {other:?}"),
    }
}

#[test]
fn a_tip_behind_the_cursor_is_already_covered() {
    let (root, runtime) = git_workspace();
    let repo_root = root.path().join("checkout");
    let baseline = git(&repo_root, &["rev-parse", "HEAD"]);
    let reviewed = commit(&repo_root, "reviewed later commit");
    completed_sweep(
        &runtime,
        &["code-review", "no-diff-expected"],
        Some(&format!(
            r#"{{"schema_version":1,"ref":"agent-main","cursor":"{reviewed}"}}"#
        )),
    );
    // The branch no longer points at the reviewed commit, but its tip is an
    // ancestor of it: there is nothing new to review.
    git(&repo_root, &["reset", "--hard", &baseline]);

    match probe_change_since_last_sweep(
        &runtime,
        &precondition(&["code-review", "no-diff-expected"], &[]),
    )
    .expect("probe")
    {
        ChangeProbe::Unchanged { cursor, tip, .. } => {
            assert_eq!(cursor, reviewed);
            assert_eq!(tip, baseline);
        }
        other => panic!("a tip behind the cursor is covered, got {other:?}"),
    }
}
