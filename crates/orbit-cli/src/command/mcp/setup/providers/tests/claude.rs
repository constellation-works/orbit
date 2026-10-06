//! ORB-12182: Claude home-scope registration must hold Claude Code's own
//! state-file lock before reading, so a concurrent Claude update is not lost.

use std::path::PathBuf;

use tempfile::tempdir;

use orbit_common::fs::io::atomic_write_text;

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::run_action;
use super::super::common::ServerLaunch;

#[test]
fn claude_home_scope_waits_for_concurrent_state_update_before_reading() {
    // Deterministic interleaving guards the lost-update incident: neither
    // init nor remove may read the old state before Claude releases its lock.
    for action in [McpAction::Init(ServerLaunch::default()), McpAction::Remove] {
        check_concurrent_update(action);
    }
}

fn check_concurrent_update(action: McpAction<'static>) {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let mcp_path = home.path().join(".claude.json");
    std::fs::write(&mcp_path, "{\n  \"userState\": \"before\"\n}\n")
        .expect("write initial Claude state");

    // Simulate Claude Code's proper-lockfile directly with mkdir/rmdir.
    // A regular-file flock here would hide the protocol mismatch.
    let mut claude_lock_path = mcp_path.clone().into_os_string();
    claude_lock_path.push(".lock");
    let claude_lock_path = PathBuf::from(claude_lock_path);

    let (lock_ready_tx, lock_ready_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let held_path = claude_lock_path.clone();
    let holder = std::thread::spawn(move || {
        std::fs::create_dir(&held_path).expect("hold Claude directory lock");
        lock_ready_tx
            .send(())
            .expect("notify that Claude lock is held");
        release_rx.recv().expect("wait for test release");
        std::fs::remove_dir(&held_path).expect("release Claude directory lock");
    });
    lock_ready_rx.recv().expect("wait for Claude lock holder");

    let worker_repo = repo.path().to_path_buf();
    let worker_home = home.path().to_path_buf();
    let worker_orbit_root = orbit_root.clone();
    let worker = std::thread::spawn(move || {
        run_action(
            action,
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
        "Claude init/remove must wait for the directory lock before reading"
    );
    atomic_write_text(
        &mcp_path,
        "{\n  \"userState\": \"during\", \"mcpServers\": {\"orbit\": {}, \"other\": {}}\n}\n",
    )
    .expect("write concurrent Claude state update");
    release_tx.send(()).expect("release Claude lock");
    holder.join().expect("join Claude lock holder");
    worker
        .join()
        .expect("join Claude init/remove")
        .expect("Claude init/remove after concurrent update");

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).expect("read final Claude state"))
            .expect("parse final Claude state");
    assert_eq!(mcp["userState"], "during");
    assert!(mcp["mcpServers"]["other"].is_object());
    assert_eq!(
        mcp["mcpServers"]["orbit"].is_object(),
        matches!(action, McpAction::Init(_))
    );
    assert!(
        !claude_lock_path.exists(),
        "Orbit must remove its directory lock"
    );
    std::fs::create_dir(&claude_lock_path).expect("Claude can acquire mkdir lock after Orbit");
    std::fs::remove_dir(&claude_lock_path).expect("release verification lock");
}
