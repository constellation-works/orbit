//! Sibling tests for `command/migrate.rs` — the versioned `.orbit/` upgrade
//! surface and the workspace-open layout pre-flight [ORB-10012].

use std::fs;
use std::path::PathBuf;

use orbit_store::contracts::{
    BreakingMigration, COMPATIBILITY_RECORD_FORMAT, CompatibilityRecord, StateComponent,
};
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;
use orbit_store::workflow::layout::SUPPORTED_LAYOUT_VERSION;

use orbit_core::OrbitRuntime;

use crate::{MigrateCommands, migrate_dry_run_at};

struct Roots {
    _temp: tempfile::TempDir,
    global_root: PathBuf,
    workspace_root: PathBuf,
}

fn temp_roots() -> Roots {
    let temp = tempfile::tempdir().expect("tempdir");
    let global_root = temp.path().join("global");
    let workspace_root = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&global_root).expect("create global root");
    fs::create_dir_all(&workspace_root).expect("create workspace root");
    Roots {
        _temp: temp,
        global_root,
        workspace_root,
    }
}

#[test]
fn dry_run_on_a_never_opened_workspace_lists_everything_pending() {
    let roots = temp_roots();

    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");

    assert_eq!(status.orbit_dir, roots.workspace_root);
    assert_eq!(status.layout_version, 0, "no marker before first open");
    assert_eq!(status.layout_supported, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(status.schema_version, 0, "no database before first open");
    assert_eq!(status.schema_supported, SUPPORTED_SCHEMA_VERSION);
    assert!(!status.pending_layout.is_empty());
    assert_eq!(status.pending_layout[0].name, "baseline");
    assert!(!status.pending_layout[0].description.is_empty());
    assert!(!status.pending_schema.is_empty());
    assert!(status.pending_total() >= 2);
    assert!(!status.newer_than_binary());
    assert!(status.applied_layout.is_empty(), "dry-run never applies");

    // Read-only: the inspection itself must not stamp or migrate anything.
    assert!(
        !roots
            .workspace_root
            .join("state")
            .join("layout.version")
            .exists()
    );
    assert!(!roots.global_root.join("orbit.db").exists());
}

#[test]
fn workspace_open_auto_migrates_and_reports_what_it_applied() {
    let roots = temp_roots();

    let runtime =
        OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root).expect("open runtime");

    // Pre-flight outcome: the open adopted the layout baseline.
    let report = runtime.layout_upgrade_report();
    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, SUPPORTED_LAYOUT_VERSION);
    assert!(!report.applied.is_empty());

    // Apply-path status: everything current, nothing pending.
    let status = runtime.migrate_status().expect("migrate status");
    assert_eq!(status.layout_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(status.schema_version, SUPPORTED_SCHEMA_VERSION);
    assert_eq!(status.pending_total(), 0);
    assert_eq!(status.applied_layout, report.applied);

    // Dry-run agrees once the workspace has been opened.
    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(status.schema_version, SUPPORTED_SCHEMA_VERSION);
    assert_eq!(status.pending_total(), 0);
}

#[test]
fn reopening_a_current_workspace_applies_nothing() {
    let roots = temp_roots();
    drop(OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root).expect("first open"));

    let runtime =
        OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root).expect("second open");
    let report = runtime.layout_upgrade_report();
    assert_eq!(report.from_version, SUPPORTED_LAYOUT_VERSION);
    assert_eq!(report.to_version, SUPPORTED_LAYOUT_VERSION);
    assert!(report.applied.is_empty());
}

#[test]
fn workspace_layout_newer_than_binary_refuses_to_open() {
    let roots = temp_roots();
    let state_dir = roots.workspace_root.join("state");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    fs::write(state_dir.join("layout.version"), "99\n").expect("write marker");

    let Err(error) = OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root) else {
        panic!("open must refuse a newer layout");
    };
    let message = error.to_string();
    assert!(message.contains("layout version 99"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");

    // The dry-run surface still inspects it and flags the mismatch.
    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, 99);
    assert!(status.newer_than_binary());
    assert!(status.pending_layout.is_empty());
}

#[test]
fn workspace_layout_newer_by_additive_migrations_opens_read_only() {
    let roots = temp_roots();
    let state_dir = roots.workspace_root.join("state");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    let newer = SUPPORTED_LAYOUT_VERSION + 1;
    fs::write(state_dir.join("layout.version"), format!("{newer}\n")).expect("write marker");
    // What a newer binary records: its extra migration is additive, and the
    // newest breaking migration is one this binary already has.
    let record = CompatibilityRecord {
        format: COMPATIBILITY_RECORD_FORMAT,
        version: newer,
        breaking: vec![BreakingMigration {
            version: SUPPORTED_LAYOUT_VERSION,
            name: "remove-task-checkout-projections".to_string(),
        }],
    };
    fs::write(
        state_dir.join("layout.compat"),
        format!("{}\n", record.encode().expect("encode record")),
    )
    .expect("write compatibility record");
    let marker_before = fs::read(state_dir.join("layout.version")).expect("read marker");

    let runtime = OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root)
        .expect("a newer-but-additive workspace must open");

    let report = runtime.layout_upgrade_report();
    assert!(report.applied.is_empty(), "an older binary applies nothing");
    let forward = report
        .forward_compatible
        .as_ref()
        .expect("the open must be recorded as forward-compatible");
    assert_eq!(forward.component, StateComponent::WorkspaceLayout);
    assert_eq!(forward.state_version, newer);

    // Read-only work is served rather than refused at open.
    runtime.list_tasks().expect("task listing must still work");
    assert_eq!(
        fs::read(state_dir.join("layout.version")).expect("read marker"),
        marker_before,
        "an older binary must not restamp a newer workspace"
    );

    // Both status surfaces report the read-only compatibility instead of
    // failing: the apply path over the open runtime...
    let status = runtime.migrate_status().expect("migrate status");
    assert_eq!(status.layout_version, newer);
    assert!(status.newer_than_binary());
    assert!(status.forward_compatible_only());
    assert_eq!(status.pending_total(), 0);

    // ...and the dry-run inspection that never opens a runtime.
    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");
    assert_eq!(status.layout_version, newer);
    assert!(status.forward_compatible_only());
    assert_eq!(
        status
            .layout_forward_compatible
            .expect("dry run reports the layout compatibility")
            .supported_version,
        SUPPORTED_LAYOUT_VERSION
    );
    assert!(status.schema_forward_compatible.is_none());
}

#[test]
fn workspace_layout_newer_by_a_breaking_migration_still_refuses() {
    let roots = temp_roots();
    let state_dir = roots.workspace_root.join("state");
    fs::create_dir_all(&state_dir).expect("mkdir state");
    let newer = SUPPORTED_LAYOUT_VERSION + 1;
    fs::write(state_dir.join("layout.version"), format!("{newer}\n")).expect("write marker");
    let record = CompatibilityRecord {
        format: COMPATIBILITY_RECORD_FORMAT,
        version: newer,
        breaking: vec![BreakingMigration {
            version: newer,
            name: "relocate-run-state".to_string(),
        }],
    };
    fs::write(
        state_dir.join("layout.compat"),
        format!("{}\n", record.encode().expect("encode record")),
    )
    .expect("write compatibility record");

    let Err(error) = OrbitRuntime::from_roots(&roots.global_root, &roots.workspace_root) else {
        panic!("a breaking-newer layout must refuse to open");
    };
    let message = error.to_string();
    assert!(
        message.contains(&format!("v{newer} (relocate-run-state)")),
        "{message}"
    );
    assert!(message.contains("upgrade orbit"), "{message}");

    let status =
        migrate_dry_run_at(&roots.global_root, &roots.workspace_root).expect("dry run status");
    assert!(status.newer_than_binary());
    assert!(!status.forward_compatible_only());
    assert!(status.layout_forward_compatible.is_none());
}
