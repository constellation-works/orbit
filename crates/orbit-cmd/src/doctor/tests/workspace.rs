use super::*;
use crate::doctor::commands::WorkspaceDoctorStatus;
use orbit_types::workflow::JobRunState;

pub(super) fn workspace_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime")
}

pub(super) fn split_root_runtime(temp: &tempfile::TempDir) -> OrbitRuntime {
    let global_root = temp.path().join("global");
    let shared_root = temp.path().join("main").join(".orbit");
    let local_root = temp.path().join("worktree").join(".orbit");
    for root in [&global_root, &shared_root, &local_root] {
        fs::create_dir_all(root).expect("create runtime root");
    }
    OrbitRuntime::from_resolved_roots(&global_root, &shared_root, &local_root)
        .expect("build split-root runtime")
}

#[cfg(unix)]
pub(super) fn reaped_child_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn child");
    let pid = child.id();
    child.wait().expect("reap child");
    pid
}

#[cfg(unix)]
pub(super) fn write_holder_lock(path: &Path, pid: u32, label: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create lock dir");
    }
    fs::write(
        path,
        serde_json::to_string(&serde_json::json!({
            "pid": pid,
            "acquired_at": Utc::now().to_rfc3339(),
            "label": label,
        }))
        .expect("serialize holder"),
    )
    .expect("write lock file");
}

#[cfg(unix)]
#[test]
pub(super) fn cleanup_preserves_a_lock_held_by_a_live_process() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &cleanup_preserves_a_lock_held_by_a_live_process,
    )) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let runtime = workspace_runtime(&temp);
    let lock_path = runtime.paths().state_dir.join(".held-op.lock");
    write_holder_lock(&lock_path, reaped_child_pid(), "stale metadata");

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open lock file");
    file.lock_exclusive().expect("hold lock");

    assert_eq!(
        runtime
            .remove_stale_lock_files()
            .expect("clean stale locks"),
        0
    );
    assert!(lock_path.exists(), "a held lock file must remain");
    file.unlock().expect("unlock lock file");
}

fn git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// `--confirm` on `gc worktrees` is governed, so the command doctor prints must
// carry the operator override. Pasted from a plain shell without it, the
// operator's own command was refused (dk-server-2, 2026-10-09).
#[cfg(unix)]
#[test]
fn worktree_reclaim_warning_prints_a_command_that_runs_from_a_plain_shell() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &worktree_reclaim_warning_prints_a_command_that_runs_from_a_plain_shell,
    )) {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let workspace_root = repo.join(".orbit");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    fs::write(
        workspace_root.join("config.toml"),
        "[worktree]\nreclaim = ['cache']\n",
    )
    .expect("write config");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    fs::write(repo.join(".gitignore"), ".orbit/\n").expect("write gitignore");
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "base"]);
    let runtime = workspace_runtime(&temp);

    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().expect("open store"),
        runtime.workspace_id().expect("workspace id"),
    );
    let at = Utc::now() - chrono::Duration::hours(1);
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            at,
            Some(serde_json::json!({"task_ids": ["T-RECLAIM"]})),
            None,
        )
        .expect("insert run");
    let worktree = workspace_root
        .join("state/worktrees")
        .join(format!("orbit-{}", run.run_id));
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-qb",
            &format!("orbit/{}", run.run_id),
            worktree.to_str().expect("utf8 worktree path"),
        ],
    );
    // Reclaim sums apparent length, so a sparse file crosses the 10 GiB
    // warning threshold without writing that much to disk.
    fs::create_dir_all(worktree.join("cache")).expect("create declared output dir");
    fs::File::create(worktree.join("cache/output"))
        .expect("create output file")
        .set_len(11 * 1024 * 1024 * 1024)
        .expect("size sparse output");
    jobs.mark_job_run_running(&run.run_id, at, reaped_child_pid())
        .expect("mark run running");
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, at, None)
        .expect("finalize run");

    let row = crate::doctor::workspace::doctor_check_worktree_reclaim(&runtime);

    assert!(
        matches!(row.status, WorkspaceDoctorStatus::Warning),
        "reclaimable output past the threshold must warn: {}",
        row.message
    );
    let action = row
        .remediation
        .as_deref()
        .expect("a reclaim warning names its next step");
    assert!(
        action.contains("ORBIT_OPERATOR=1 orbit gc worktrees --reclaim --confirm"),
        "the reclaim command doctor prints must carry the operator override, or pasting it from a plain shell is refused (dk-server-2, 2026-10-09): {action}"
    );
}
