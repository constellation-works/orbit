//! ORB-12182: Claude home-scope registration must hold Claude Code's own
//! state-file lock before reading, so a concurrent Claude update is not lost.

use std::path::PathBuf;

use tempfile::tempdir;

use orbit_common::fs::io::{FileLockOptions, acquire_exclusive_file_lock, atomic_write_text};

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::run_action;
use super::super::common::ServerLaunch;

#[test]
fn claude_home_scope_waits_for_concurrent_state_update_before_reading() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let mcp_path = home.path().join(".claude.json");
    std::fs::write(&mcp_path, "{\n  \"userState\": \"before\"\n}\n")
        .expect("write initial Claude state");

    // Simulate Claude Code itself, which locks `<mcp_path>.lock` — the full
    // file name with `.lock` appended, not Orbit's usual dot-prefixed
    // sibling. Holding that literal path (independent of the production
    // helper) is what proves Orbit actually waits on Claude Code's own lock
    // rather than a differently-named file neither process contends on.
    let mut claude_lock_path = mcp_path.clone().into_os_string();
    claude_lock_path.push(".lock");
    let claude_lock_path = PathBuf::from(claude_lock_path);

    let (lock_ready_tx, lock_ready_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let held_path = claude_lock_path.clone();
    let holder = std::thread::spawn(move || {
        let _guard = acquire_exclusive_file_lock(
            &held_path,
            "test Claude Code writer",
            FileLockOptions::default(),
        )
        .expect("hold Claude lock");
        lock_ready_tx
            .send(())
            .expect("notify that Claude lock is held");
        release_rx.recv().expect("wait for test release");
    });
    lock_ready_rx.recv().expect("wait for Claude lock holder");

    let worker_repo = repo.path().to_path_buf();
    let worker_home = home.path().to_path_buf();
    let worker_orbit_root = orbit_root.clone();
    let worker = std::thread::spawn(move || {
        run_action(
            McpAction::Init(ServerLaunch::default()),
            &worker_repo,
            &worker_orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
            Some(worker_home),
            ScopeArg::Home,
        )
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "Claude init must wait for the state-file lock before reading"
    );
    atomic_write_text(&mcp_path, "{\n  \"userState\": \"during\"\n}\n")
        .expect("write concurrent Claude state update");
    release_tx.send(()).expect("release Claude lock");
    holder.join().expect("join Claude lock holder");
    worker
        .join()
        .expect("join Claude init")
        .expect("Claude init after concurrent update");

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).expect("read final Claude state"))
            .expect("parse final Claude state");
    assert_eq!(mcp["userState"], "during");
    assert!(mcp["mcpServers"]["orbit"].is_object());
}
