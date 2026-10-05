use super::*;

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
