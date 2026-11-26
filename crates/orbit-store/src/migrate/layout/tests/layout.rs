//! Sibling tests for the workspace-layout migration registry (ORB-10012).

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;

use crate::contract::MigrationCompatibility;

use crate::migrate::layout::current_layout_version;
use crate::migrate::layout::registry::LayoutMigration;
use crate::migrate::layout::upgrade::upgrade_with;

const TOY_V2_REGISTRY: &[LayoutMigration] = &[
    LayoutMigration {
        version: 1,
        name: "baseline",
        description: "adopt the versioned layout",
        compat: MigrationCompatibility::Additive,
        apply: |_| Ok(()),
    },
    LayoutMigration {
        version: 2,
        name: "notes-into-subdir",
        description: "move notes.txt under notes/",
        compat: MigrationCompatibility::Additive,
        apply: toy_v2_apply,
    },
];

fn temp_orbit_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("create temp .orbit dir")
}

// ── test-only v2 registry: exercises a real layout change end to end ──

fn toy_v2_apply(orbit_dir: &Path) -> Result<(), OrbitError> {
    // Idempotent rename: move legacy `notes.txt` under `notes/` (staged
    // write-new-then-swap shape a real migration would use).
    let legacy = orbit_dir.join("notes.txt");
    let target_dir = orbit_dir.join("notes");
    fs::create_dir_all(&target_dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    if legacy.exists() {
        fs::rename(&legacy, target_dir.join("notes.txt"))
            .map_err(|e| OrbitError::Io(e.to_string()))?;
    }
    Ok(())
}

fn failing_apply(_orbit_dir: &Path) -> Result<(), OrbitError> {
    Err(OrbitError::Execution("boom".to_string()))
}

#[test]
fn failed_migration_keeps_the_marker_at_the_last_applied_version() {
    let temp = temp_orbit_dir();
    const FAILING_REGISTRY: &[LayoutMigration] = &[
        LayoutMigration {
            version: 1,
            name: "baseline",
            description: "adopt",
            compat: MigrationCompatibility::Additive,
            apply: |_| Ok(()),
        },
        LayoutMigration {
            version: 2,
            name: "explodes",
            description: "always fails",
            compat: MigrationCompatibility::Additive,
            apply: failing_apply,
        },
    ];

    let error = upgrade_with(temp.path(), FAILING_REGISTRY).expect_err("v2 must fail");
    let message = error.to_string();
    assert!(message.contains("v2"), "{message}");
    assert!(message.contains("explodes"), "{message}");
    // v1 landed and was recorded; the failed v2 did not advance the marker,
    // so a fixed binary resumes exactly at v2.
    assert_eq!(current_layout_version(temp.path()).expect("version"), 1);

    let report = upgrade_with(temp.path(), TOY_V2_REGISTRY).expect("resume with fixed registry");
    assert_eq!(report.from_version, 1);
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].version, 2);
}
