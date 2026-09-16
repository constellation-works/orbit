//! Reuse and invalidation rules of the server's per-workspace runtime cache.
//!
//! The cache is exercised with a counter in place of a runtime: what matters
//! here is *when* it decides to build, which is independent of what it builds.
//! The crate's `tests/mcp_roundtrip.rs` covers the same rules against real
//! runtimes over the production stdio transport.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;
use orbit_cmd::registry_runtime::ResolvedWorkspaceSelection;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceStatus};

use super::WorkspaceRuntimeCache;

/// A global root whose registry, host identity, and workspace config all exist,
/// so every stamped input starts out readable.
struct Root {
    temp: tempfile::TempDir,
}

impl Root {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("global root");
        let root = Self { temp };
        std::fs::write(root.registry_path(), "{}").expect("registry file");
        std::fs::write(root.path().join("host.toml"), "schema_version = 2\n")
            .expect("host identity");
        std::fs::create_dir_all(root.orbit_dir()).expect("orbit dir");
        std::fs::write(root.workspace_config_path(), "workspace_id: ws_local\n")
            .expect("workspace config");
        root
    }

    fn path(&self) -> &Path {
        self.temp.path()
    }

    fn registry_path(&self) -> PathBuf {
        self.path().join("workspaces.json")
    }

    fn orbit_dir(&self) -> PathBuf {
        self.path().join("checkout/.orbit")
    }

    fn workspace_config_path(&self) -> PathBuf {
        self.orbit_dir().join("config.yaml")
    }

    fn selection(&self) -> ResolvedWorkspaceSelection {
        let now = Utc::now();
        ResolvedWorkspaceSelection {
            workspace: Workspace {
                id: "ws_cache".to_string(),
                name: "cache".to_string(),
                owner_machine_id: Some("hm_local".to_string()),
                git_remote: None,
                ship_mode: None,
                base_branch: "main".to_string(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            },
            checkout: WorkspaceCheckout::owner(
                "ws_cache".to_string(),
                self.path().join("checkout"),
                self.orbit_dir(),
            ),
            local_root: self.orbit_dir(),
        }
    }
}

/// Counts builds and hands back a distinguishable value per build.
#[derive(Default)]
struct Builds(AtomicUsize);

impl Builds {
    fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    fn next(&self) -> Result<usize, orbit_common::OrbitError> {
        Ok(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

/// Writes `bytes` over a stamped input. Lengths differ between writes so the
/// change is visible regardless of filesystem timestamp granularity.
fn overwrite(path: &Path, bytes: &str) {
    std::fs::write(path, bytes).expect("overwrite a stamped file");
}

#[test]
fn repeated_calls_for_one_workspace_reuse_a_single_runtime() {
    let root = Root::new();
    let selection = root.selection();
    let cache = WorkspaceRuntimeCache::<usize>::default();
    let builds = Builds::default();

    let first = cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("first build");
    let second = cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("reuse");

    assert_eq!(builds.count(), 1, "the second call must not build");
    assert!(std::sync::Arc::ptr_eq(&first, &second));
}

#[test]
fn a_registry_edit_between_calls_rebuilds_the_runtime() {
    let root = Root::new();
    let selection = root.selection();
    let cache = WorkspaceRuntimeCache::<usize>::default();
    let builds = Builds::default();

    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("first build");
    // Any registry write invalidates, not only one that changes this
    // workspace's own record: registering another workspace, renaming a
    // sibling, or a validation save all rewrite this file.
    overwrite(&root.registry_path(), "{\"schema_version\":3}");
    let rebuilt = cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("rebuild after the registry changed");

    assert_eq!(builds.count(), 2);
    assert_eq!(*rebuilt, 1, "the second build must be the live entry");

    // And the rebuilt entry is itself reusable while the file stays put.
    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("reuse the rebuilt entry");
    assert_eq!(builds.count(), 2);
}

#[test]
fn a_host_identity_or_workspace_config_edit_rebuilds_the_runtime() {
    let root = Root::new();
    let selection = root.selection();
    let cache = WorkspaceRuntimeCache::<usize>::default();
    let builds = Builds::default();

    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("first build");
    // The task prefix and machine identity a runtime carries come from
    // host.toml; its task partition comes from the checkout's config.yaml.
    overwrite(
        &root.path().join("host.toml"),
        "schema_version = 2\ntask_prefix = \"NEW\"\n",
    );
    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("rebuild after the host identity changed");
    assert_eq!(builds.count(), 2);

    overwrite(
        &root.workspace_config_path(),
        "workspace_id: ws_relocated\n",
    );
    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("rebuild after the workspace config changed");
    assert_eq!(builds.count(), 3);
}

#[test]
fn a_rebound_workspace_record_is_not_served_from_the_cache() {
    let root = Root::new();
    let selection = root.selection();
    let cache = WorkspaceRuntimeCache::<usize>::default();
    let builds = Builds::default();

    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("first build");

    // Same workspace ID, different registered facts. Keying by ID alone would
    // serve a runtime bound to the wrong branch, owner, or checkout.
    let mut rebound = selection.clone();
    rebound.workspace.base_branch = "release".to_string();
    cache
        .resolve(root.path(), &rebound, || builds.next())
        .expect("rebuild after a workspace rebind");
    assert_eq!(builds.count(), 2);

    let mut relocated = rebound.clone();
    relocated.checkout.repo_root = root.path().join("elsewhere");
    cache
        .resolve(root.path(), &relocated, || builds.next())
        .expect("rebuild after a checkout relocation");
    assert_eq!(builds.count(), 3);

    // The live entry is the last one published, and the superseded selections
    // are not served again.
    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("rebuild for the original selection");
    assert_eq!(builds.count(), 4);
}

#[test]
fn a_failed_build_is_not_cached() {
    let root = Root::new();
    let selection = root.selection();
    let cache = WorkspaceRuntimeCache::<usize>::default();
    let builds = Builds::default();

    let error = cache
        .resolve(root.path(), &selection, || {
            Err(orbit_common::OrbitError::Execution(
                "store open".to_string(),
            ))
        })
        .expect_err("the build failure must surface");
    assert!(matches!(error, orbit_common::OrbitError::Execution(_)));

    cache
        .resolve(root.path(), &selection, || builds.next())
        .expect("a later call builds");
    assert_eq!(builds.count(), 1);
}
