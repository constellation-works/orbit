// Migrated from file/skill_store.rs per ORB-00231
use std::fs;
use std::path::Path;

use orbit_common::OrbitError;
use tempfile::tempdir;

use super::super::*;

#[test]
fn load_rejects_traversal_and_absolute_skill_ids_without_leaving_catalog() {
    let parent = tempdir().expect("sandbox tempdir");
    let catalog_root = parent.path().join("catalog");
    fs::create_dir(&catalog_root).expect("create catalog root");
    write_skill(parent.path(), "escape", "escaped purpose");

    let catalog = SkillCatalog::new(catalog_root);

    let traversal = catalog
        .load("../escape")
        .expect_err("relative traversal must fail");
    assert!(
        matches!(traversal, OrbitError::InvalidInput(_)),
        "expected InvalidInput for ../escape, got {traversal:?}"
    );

    let absolute = catalog
        .load("/etc")
        .expect_err("absolute skill id must fail");
    assert!(
        matches!(absolute, OrbitError::InvalidInput(_)),
        "expected InvalidInput for /etc, got {absolute:?}"
    );

    let decoy = SkillCatalog::new(parent.path().to_path_buf());
    assert_eq!(
        decoy
            .load("escape")
            .expect("decoy skill must be loadable from its own root")
            .sections
            .purpose,
        "escaped purpose",
        "InvalidInput is not a missing-file miss: the sibling decoy is a valid skill"
    );
}

fn write_skill(root: &Path, id: &str, purpose: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("create skill dir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: test skill\n---\n\n# Purpose\n\n{purpose}\n"),
    )
    .expect("write skill");
}
