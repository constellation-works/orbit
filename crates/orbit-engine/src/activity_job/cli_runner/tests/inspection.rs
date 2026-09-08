use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use orbit_common::fs::git::run_git;
use orbit_types::workflow::activity_job::V2AuditEventKind;
use serde_json::json;
use tempfile::{TempDir, tempdir};

use super::super::super::audit_writer::V2AuditWriter;
use super::super::inspection::SourceInspection;
use super::super::run_cli_backend;
use super::test_support::{RecordingSink, TestHost, test_agent_loop_spec, write_executable};

fn git(root: &Path, args: &[&str]) -> String {
    let output = run_git(root, args).unwrap();
    assert!(output.success, "{args:?}: {}", output.stderr);
    output.stdout.trim().to_string()
}

fn repository() -> (TempDir, String) {
    let root = tempdir().unwrap();
    git(root.path(), &["init", "--quiet"]);
    git(root.path(), &["config", "user.name", "Orbit Test"]);
    git(root.path(), &["config", "user.email", "test@example.com"]);
    git(root.path(), &["config", "commit.gpgsign", "false"]);
    fs::write(root.path().join("target.txt"), "pinned\n").unwrap();
    // Repositories legitimately track symlinks (Orbit's own per-crate
    // AGENTS.md files are one), so every slot carries them into its checkout.
    std::os::unix::fs::symlink("target.txt", root.path().join("linked.txt")).unwrap();
    git(root.path(), &["add", "target.txt", "linked.txt"]);
    git(root.path(), &["commit", "--quiet", "-m", "initial"]);
    let revision = git(root.path(), &["rev-parse", "HEAD"]);
    (root, revision)
}

fn inspect(root: &Path, revision: &str) -> SourceInspection {
    SourceInspection::from_input(
        &json!({"inspection_revision": revision}),
        Some(root),
        Some("reviewer"),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn snapshot_stays_pinned_and_live_leases_are_never_reclaimed() {
    let (repo, revision) = repository();
    let registered = git(repo.path(), &["worktree", "list", "--porcelain"]);
    let first = inspect(repo.path(), &revision);
    let first_root = first.root().to_path_buf();
    fs::write(repo.path().join("target.txt"), "later\n").unwrap();
    git(repo.path(), &["commit", "-am", "later"]);
    let second = inspect(repo.path(), &revision);
    assert_ne!(first.root(), second.root());
    assert_eq!(
        fs::read_to_string(first.root().join("target.txt")).unwrap(),
        "pinned\n"
    );
    assert_eq!(git(second.root(), &["rev-parse", "HEAD"]), revision);
    first.verify().unwrap();
    let bound = second.bind_input(&json!({"workspace_path": repo.path()}));
    assert_eq!(bound["workspace_path"], second.root().display().to_string());
    assert_eq!(bound["repo_root"], bound["workspace_path"]);
    let second_root = second.root().to_path_buf();
    drop(second);
    assert!(!second_root.exists());
    assert!(first_root.exists());
    drop(first);
    assert!(!first_root.exists());
    // The primary's HEAD changed above, but no worktree registry entry was added.
    assert_eq!(
        registered
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .collect::<Vec<_>>(),
        git(repo.path(), &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .collect::<Vec<_>>()
    );
}

/// The slot must not sit under authoritative Git metadata: host protection
/// refuses every symlink there, and a checkout of tracked content has them.
#[test]
fn slots_carry_tracked_symlinks_and_stay_outside_the_git_metadata_tree() {
    let (repo, revision) = repository();
    let common = git(
        repo.path(),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let snapshot = inspect(repo.path(), &revision);

    let linked = snapshot.root().join("linked.txt");
    assert!(
        fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&linked).unwrap(), "pinned\n");
    snapshot.verify().unwrap();

    assert!(
        !snapshot.root().starts_with(&common),
        "{} is inside the common Git directory {common}",
        snapshot.root().display()
    );
    assert!(
        snapshot
            .root()
            .starts_with(repo.path().join(".orbit/state/source-inspections-v1")),
        "{}",
        snapshot.root().display()
    );
}

/// A crash could abandon a checkout in the pool this module used to keep
/// inside the common Git directory, where host protection refuses its tracked
/// symlinks. The current pool never leases that slot again, so creating an
/// inspection retires the whole legacy pool instead.
#[test]
fn creating_an_inspection_retires_an_abandoned_in_metadata_pool() {
    let (repo, revision) = repository();
    let common = git(
        repo.path(),
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let legacy = Path::new(&common).join("orbit-source-inspections-v1");
    let abandoned = legacy.join("0/checkout");
    fs::create_dir_all(&abandoned).unwrap();
    fs::write(legacy.join("0/owner"), "orbit-source-inspection-v1\n").unwrap();
    std::os::unix::fs::symlink("target.txt", abandoned.join("linked.txt")).unwrap();

    let snapshot = inspect(repo.path(), &revision);
    assert!(!legacy.exists());
    drop(snapshot);

    // An entry this module did not write stays for an operator to inspect.
    let foreign = legacy.join("0");
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("owner"), "another tool\n").unwrap();
    let snapshot = inspect(repo.path(), &revision);
    assert!(foreign.exists());
    drop(snapshot);
}

#[test]
fn rejects_invalid_revision_and_write_profiles_and_detects_edits() {
    let (repo, revision) = repository();
    assert!(
        SourceInspection::from_input(
            &json!({"inspection_revision": null}),
            Some(repo.path()),
            Some("reviewer")
        )
        .is_err()
    );
    let non_git = tempdir().unwrap();
    assert!(
        SourceInspection::from_input(
            &json!({"inspection_revision": null}),
            Some(non_git.path()),
            Some("reviewer")
        )
        .unwrap()
        .is_none()
    );
    for (revision, profile) in [
        ("HEAD", Some("reviewer")),
        (revision.as_str(), None),
        (revision.as_str(), Some("implementer")),
    ] {
        assert!(
            SourceInspection::from_input(
                &json!({"inspection_revision": revision}),
                Some(repo.path()),
                profile
            )
            .is_err()
        );
    }
    let snapshot = inspect(repo.path(), &revision);
    fs::write(snapshot.root().join("target.txt"), "changed").unwrap();
    assert!(snapshot.verify().is_err());
    assert!(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision, "source_revision": "different"}),
            Some(repo.path()),
            Some("reviewer"),
        )
        .is_err()
    );
}

#[test]
fn inspection_capacity_is_bounded_and_released_slots_can_be_reused() {
    let (repo, revision) = repository();
    let mut snapshots: Vec<_> = (0..16).map(|_| inspect(repo.path(), &revision)).collect();
    assert!(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision}),
            Some(repo.path()),
            Some("reviewer")
        )
        .is_err()
    );
    let released = snapshots.pop().unwrap();
    let released_path = released.root().to_path_buf();
    drop(released);
    let next = inspect(repo.path(), &revision);
    assert_eq!(next.root(), released_path);
    for snapshot in snapshots {
        snapshot.verify().unwrap();
    }
}

#[test]
fn crash_child() {
    let Some(repo) = std::env::var_os("ORBIT_INSPECTION_CRASH_FIXTURE") else {
        return;
    };
    let root = Path::new(&repo);
    let revision = git(root, &["rev-parse", "HEAD"]);
    let snapshot = inspect(root, &revision);
    fs::write(snapshot.root().join("abandoned"), "crashed").unwrap();
    // Exercise kernel lease release without running any Rust destructors.
    std::process::exit(0);
}

#[test]
fn retry_reclaims_crash_leftovers_and_keeps_another_live_snapshot() {
    let (repo, revision) = repository();
    let live = inspect(repo.path(), &revision);
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "activity_job::cli_runner::tests::inspection::crash_child",
            "--nocapture",
        ])
        .env("ORBIT_INSPECTION_CRASH_FIXTURE", repo.path())
        .status()
        .unwrap();
    assert!(status.success());
    let abandoned = live
        .root()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("1/checkout/abandoned");
    assert!(abandoned.exists());
    let retry = inspect(repo.path(), &revision);
    assert!(!abandoned.exists());
    assert_ne!(retry.root(), live.root());
    live.verify().unwrap();
}

#[test]
fn cleanup_refuses_a_registered_worktree_in_a_slot() {
    let (repo, revision) = repository();
    let snapshot = inspect(repo.path(), &revision);
    let root = snapshot.root().to_path_buf();
    drop(snapshot);
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            root.to_str().unwrap(),
            &revision,
        ],
    );
    assert!(
        SourceInspection::from_input(
            &json!({"inspection_revision": revision}),
            Some(repo.path()),
            Some("reviewer")
        )
        .is_err()
    );
    assert!(root.join("target.txt").exists());
    git(repo.path(), &["worktree", "remove", root.to_str().unwrap()]);
}

#[test]
fn dispatch_binds_native_reads_envelope_and_cwd_but_retains_task_authority() {
    let (repo, revision) = repository();
    fs::write(repo.path().join("target.txt"), "primary dirty\n").unwrap();
    let index = fs::read(repo.path().join(".git/index")).unwrap();
    let scripts = tempdir().unwrap();
    let script = scripts.path().join("codex");
    write_executable(
        &script,
        r#"#!/bin/sh
envelope=$(cat)
if [ "$(cat target.txt)" != pinned ]; then
  echo "native source read did not use the pinned checkout" >&2
  exit 11
fi
if [ "$ORBIT_WORKSPACE" != ws_owner ]; then
  echo "managed workspace identity was not retained" >&2
  exit 12
fi
if [ "$ORBIT_REGISTRY_ROOT" != /authoritative/registry ]; then
  echo "managed registry identity was not retained" >&2
  exit 13
fi
if ! printf '%s' "$envelope" | grep -F "$(pwd)" >/dev/null; then
  echo "envelope did not retain the dispatched cwd" >&2
  exit 14
fi
printf '%s\n' '{"schemaVersion":1,"status":"success","result":{},"error":null}'
"#,
    );
    let mut host = TestHost::with_command(script.display().to_string());
    host.workspace_root = Some(repo.path().to_path_buf());
    host.task_context = Some(json!({"workspace_path": repo.path(), "repo_root": repo.path()}));
    host.orbit_workspace_selector = Some("ws_owner".into());
    host.orbit_registry_root = Some("/authoritative/registry".into());
    let sink = Arc::new(RecordingSink::default());
    let audit = Arc::new(V2AuditWriter::new("inspection-test", "codex", sink.clone()));
    let outcome = run_cli_backend(
        &host,
        &test_agent_loop_spec(Duration::from_secs(5)),
        "test_activity",
        "inspection-test",
        audit.clone(),
        &json!({"workspace_path": repo.path(), "inspection_revision": revision}),
        Some("reviewer"),
    )
    .unwrap();
    let stderr = outcome
        .output
        .get("stderr_blob_ref")
        .and_then(serde_json::Value::as_str)
        .and_then(|reference| sink.blob(reference))
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    assert!(
        outcome.success,
        "dispatch failed: message={:?}, stderr={stderr:?}",
        outcome.message
    );
    let events = audit.events_snapshot().unwrap();
    let cwd = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { cwd, .. } => cwd.as_deref(),
            _ => None,
        })
        .unwrap();
    assert_ne!(Path::new(cwd), repo.path());
    let envelope_ref = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { stdin_blob_ref, .. } => {
                stdin_blob_ref.as_deref()
            }
            _ => None,
        })
        .unwrap();
    let stdin = String::from_utf8(sink.blob(envelope_ref).unwrap()).unwrap();
    let (_, encoded) = stdin.split_once("Execution envelope:\n").unwrap();
    let envelope: serde_json::Value = serde_json::from_str(encoded).unwrap();
    for field in ["input", "task"] {
        assert_eq!(envelope[field]["workspace_path"], cwd);
        assert_eq!(envelope[field]["repo_root"], cwd);
        assert_eq!(envelope[field]["source_revision"], revision);
    }
    assert!(
        !Path::new(cwd).exists(),
        "dispatch must release inspection on return"
    );
    assert_eq!(fs::read(repo.path().join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(repo.path().join("target.txt")).unwrap(),
        "primary dirty\n"
    );
}

#[test]
fn dispatch_failure_and_timeout_release_the_inspection_checkout() {
    let (repo, revision) = repository();
    let scripts = tempdir().unwrap();
    for ending in ["exit 7", "sleep 5"] {
        let script = scripts.path().join("codex");
        write_executable(&script, &format!("#!/bin/sh\ncat >/dev/null\n{ending}\n"));
        let mut host = TestHost::with_command(script.display().to_string());
        host.workspace_root = Some(repo.path().to_path_buf());
        let audit = Arc::new(V2AuditWriter::new(
            "inspection-failure",
            "codex",
            Arc::new(RecordingSink::default()),
        ));
        let result = run_cli_backend(
            &host,
            &test_agent_loop_spec(Duration::from_secs(1)),
            "test_activity",
            "inspection-failure",
            audit.clone(),
            &json!({"workspace_path": repo.path(), "inspection_revision": revision}),
            Some("reviewer"),
        );
        assert!(result.is_err() || !result.unwrap().success);
        let events = audit.events_snapshot().unwrap();
        let cwd = events
            .iter()
            .find_map(|event| match &event.kind {
                V2AuditEventKind::CliInvocationStarted { cwd, .. } => cwd.as_deref(),
                _ => None,
            })
            .unwrap();
        assert!(!Path::new(cwd).exists());
    }
}
