//! Unit tests for multi-workspace state and default-workspace resolution
//! (ORB-00030).

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_types::workspace::{WorkspaceRegistry, WorkspaceStatus};

use super::dashboard::{checkout, workspace};
use crate::state::{DashboardState, RegistrySource};

fn write_registry(global_root: &Path, ids: &[&str]) {
    let mut registry = WorkspaceRegistry::default();
    for id in ids {
        registry
            .workspaces
            .push(workspace(id, WorkspaceStatus::Active));
        let root = PathBuf::from(format!("/nonexistent/{id}"));
        registry
            .checkouts
            .push(checkout(id, root.to_str().expect("utf8 path")));
    }
    orbit_registry::workspace_registry::save_registry_to(
        &registry,
        &global_root.join("workspaces.json"),
    )
    .expect("save registry");
}

fn registry_state(ids: &[&str]) -> (tempfile::TempDir, DashboardState) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let global_root = tmp.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");
    write_registry(&global_root, ids);
    let source = RegistrySource::new(global_root.join("workspaces.json"), None, None);
    let state = DashboardState::from_registry(global_root, source).expect("from_registry");
    (tmp, state)
}

fn listed_ids(state: &DashboardState) -> Vec<String> {
    let _pinned = state.pin();
    let mut ids: Vec<String> = state
        .entries()
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    ids.sort();
    ids
}

/// Unchanged `workspaces.json` must not be re-read on later `pin` calls.
#[test]
fn pin_skips_registry_load_when_mtime_and_len_are_unchanged() {
    let (_tmp, state) = registry_state(&["alpha"]);
    assert_eq!(state.registry_load_count(), 1, "eager from_registry load");

    let _ = state.pin();
    let _ = state.pin();
    assert_eq!(
        state.registry_load_count(),
        1,
        "matching fingerprint must skip load"
    );
    assert_eq!(listed_ids(&state), vec!["alpha".to_string()]);
}

/// Steady-state `pin` must not wait on `refresh_lock`.
#[test]
fn pin_does_not_serialize_on_refresh_lock_when_registry_unchanged() {
    let (_tmp, state) = registry_state(&["alpha"]);
    assert_eq!(state.registry_load_count(), 1);

    let _guard = state.lock_refresh();
    let (tx, rx) = std::sync::mpsc::channel();
    let pinned = state.clone();
    std::thread::spawn(move || {
        let _ = pinned.pin();
        let _ = tx.send(());
    });
    rx.recv_timeout(Duration::from_millis(750))
        .expect("pin must not wait on refresh_lock when the registry fingerprint is unchanged");
}

/// A rewritten registry file is visible on the next `pin` without waiting for
/// a background tick.
#[test]
fn pin_picks_up_registry_rewrite_on_the_next_request() {
    let (_tmp, state) = registry_state(&["alpha"]);
    assert_eq!(listed_ids(&state), vec!["alpha".to_string()]);
    assert_eq!(state.registry_load_count(), 1);

    write_registry(state.global_root(), &["alpha", "beta"]);
    assert_eq!(
        listed_ids(&state),
        vec!["alpha".to_string(), "beta".to_string()]
    );
    assert_eq!(
        state.registry_load_count(),
        2,
        "changed mtime/len must reload"
    );
}
