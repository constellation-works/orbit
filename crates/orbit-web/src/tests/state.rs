//! Unit tests for multi-workspace state and default-workspace resolution
//! (ORB-00030).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};

use crate::state::{DashboardState, RegistrySource, WsEntry};
use crate::{default_workspace_for_cwd, default_workspace_selection};

fn workspace(id: &str, status: WorkspaceStatus) -> Workspace {
    let now = Utc::now();
    Workspace {
        id: id.to_string(),
        name: id.to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status,
        created_at: now,
        updated_at: now,
    }
}

fn checkout(id: &str, root: &str) -> WorkspaceCheckout {
    WorkspaceCheckout::owner(
        id.to_string(),
        PathBuf::from(root),
        PathBuf::from(root).join(".orbit"),
    )
}

#[test]
fn single_mode_exposes_one_default_workspace() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));

    assert_eq!(state.entries().len(), 1);
    assert_eq!(state.default_workspace().as_deref(), Some("default"));
    assert!(state.runtime_for("default").is_ok());
    assert!(state.runtime_for("unknown").is_err());
}

#[test]
fn global_mode_rejects_inactive_and_unknown_workspaces() {
    let entries = vec![WsEntry {
        id: "stale".to_string(),
        name: "stale".to_string(),
        repo_root: PathBuf::from("/nonexistent"),
        orbit_dir: PathBuf::from("/nonexistent/.orbit"),
        binding: None,
        active: false,
    }];
    let state = DashboardState::global(PathBuf::from("/nonexistent"), entries, None);

    // Inactive entries are listed but never built.
    assert_eq!(state.entries().len(), 1);
    assert!(state.runtime_for("stale").is_err());
    // Unknown ids are rejected outright.
    assert!(state.runtime_for("ghost").is_err());
    assert_eq!(state.default_workspace(), None);
}

#[test]
fn default_workspace_for_cwd_picks_longest_active_prefix() {
    let registry = WorkspaceRegistry {
        workspaces: vec![
            workspace("outer", WorkspaceStatus::Active),
            workspace("inner", WorkspaceStatus::Active),
            workspace("stale", WorkspaceStatus::Invalid),
        ],
        checkouts: vec![
            checkout("outer", "/repos"),
            checkout("inner", "/repos/inner"),
            checkout("stale", "/repos/inner/sub"),
        ],
        ..Default::default()
    };

    // Deepest active workspace wins; the still-deeper inactive one is ignored.
    assert_eq!(
        default_workspace_for_cwd(&registry, Path::new("/repos/inner/sub/pkg")),
        Some("inner".to_string())
    );
    // Outside any registered root -> no default (frontend opens the aggregate).
    assert_eq!(
        default_workspace_for_cwd(&registry, Path::new("/elsewhere")),
        None
    );
}

#[test]
fn default_workspace_selection_selector_beats_cwd() {
    let registry = WorkspaceRegistry {
        workspaces: vec![
            workspace("outer", WorkspaceStatus::Active),
            workspace("inner", WorkspaceStatus::Active),
        ],
        checkouts: vec![
            checkout("outer", "/repos"),
            checkout("inner", "/repos/inner"),
        ],
        ..Default::default()
    };

    // cwd would resolve to "outer", but an explicit --workspace pointing at
    // "inner" takes priority (ORB-10029 regression fix: this is the only
    // signal `orbit web connect` can pass through for the remote's cwd,
    // which is the SSH user's home directory, not any workspace).
    assert_eq!(
        default_workspace_selection(
            &registry,
            Some("/repos/inner"),
            Some(Path::new("/repos/pkg")),
        ),
        Some("inner".to_string())
    );
    // The same selector by registered name, which is what a non-path
    // `--workspace` value resolves through.
    assert_eq!(
        default_workspace_selection(&registry, Some("inner"), Some(Path::new("/repos/pkg"))),
        Some("inner".to_string())
    );
}

#[test]
fn default_workspace_selection_unmatched_selector_falls_back_to_none() {
    let registry = WorkspaceRegistry {
        workspaces: vec![workspace("outer", WorkspaceStatus::Active)],
        checkouts: vec![checkout("outer", "/repos")],
        ..Default::default()
    };

    // An unmatched --workspace falls back to "All workspaces" (None) even
    // though cwd would otherwise resolve to a real workspace — it must not
    // silently fall through to the cwd-based default, and must not error or
    // auto-register.
    assert_eq!(
        default_workspace_selection(&registry, Some("/nowhere"), Some(Path::new("/repos/pkg")),),
        None
    );
    assert_eq!(
        default_workspace_selection(
            &registry,
            Some("no-such-name"),
            Some(Path::new("/repos/pkg"))
        ),
        None
    );
}

#[test]
fn default_workspace_selection_resolves_relative_selector_against_cwd() {
    // ORB-10053: a relative path selector (e.g. `--workspace .`) must resolve
    // against cwd and canonicalize before the prefix-match against registered
    // roots (which are canonical absolute paths). Prior behavior compared the
    // raw relative path lexically and always missed, silently falling back to
    // "All workspaces" instead of preselecting the workspace.
    let tmp = tempfile::tempdir().expect("tempdir");
    let parent = tmp.path().canonicalize().expect("canonicalize tmp");
    let ws_dir = parent.join("my_workspace");
    std::fs::create_dir(&ws_dir).expect("create ws dir");
    let canonical_ws_root = ws_dir.canonicalize().expect("canonicalize ws root");

    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: "my_workspace".to_string(),
            name: "my_workspace".to_string(),
            owner_machine_id: None,
            git_remote: None,
            ship_mode: None,
            base_branch: "main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }],
        checkouts: vec![WorkspaceCheckout::owner(
            "my_workspace".to_string(),
            canonical_ws_root.clone(),
            canonical_ws_root.join(".orbit"),
        )],
        ..Default::default()
    };

    // A relative path selector, resolved against a cwd that is the parent of
    // the workspace, must preselect the workspace — matching the behavior of
    // passing the equivalent absolute path. (The registered name here is
    // `my_workspace` too, so the relative-path case is asserted through a
    // sibling directory name that no registered name matches.)
    assert_eq!(
        default_workspace_selection(&registry, Some("./my_workspace"), Some(&parent)),
        Some("my_workspace".to_string())
    );
    assert_eq!(
        default_workspace_selection(
            &registry,
            Some(&canonical_ws_root.to_string_lossy()),
            Some(&parent)
        ),
        Some("my_workspace".to_string())
    );
}

#[test]
fn default_workspace_selection_no_selector_falls_back_to_cwd() {
    let registry = WorkspaceRegistry {
        workspaces: vec![workspace("outer", WorkspaceStatus::Active)],
        checkouts: vec![checkout("outer", "/repos")],
        ..Default::default()
    };

    assert_eq!(
        default_workspace_selection(&registry, None, Some(Path::new("/repos/pkg"))),
        Some("outer".to_string())
    );
    assert_eq!(default_workspace_selection(&registry, None, None), None);
}

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
