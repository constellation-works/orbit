// Migrated from file/skill_store.rs per ORB-00231
use tempfile::tempdir;

use super::super::*;

#[test]
fn layered_catalog_uses_merge_by_key_precedence() {
    let workspace = tempdir().expect("workspace tempdir");
    let global = tempdir().expect("global tempdir");

    write_skill(global.path(), "orbit", "global skill");
    write_skill(global.path(), "orbit-search", "global search");
    write_skill(workspace.path(), "orbit", "workspace override");

    let catalog =
        SkillCatalog::layered(workspace.path().to_path_buf(), global.path().to_path_buf());

    assert_eq!(catalog.strategy(), ScopeStrategy::MergeByKey);
    assert_eq!(
        catalog
            .load("orbit")
            .expect("load override")
            .sections
            .purpose,
        "workspace override"
    );
    assert_eq!(
        catalog
            .load("orbit-search")
            .expect("load global fallback")
            .sections
            .purpose,
        "global search"
    );

    let ids = catalog
        .list()
        .expect("list skills")
        .into_iter()
        .map(|skill| skill.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["orbit", "orbit-search"]);

    let rows = catalog.doctor().expect("doctor layered catalog");
    assert_eq!(rows.len(), 2, "the workspace shadow is counted once");
    assert!(
        rows.iter()
            .all(|row| row.status == SkillCatalogDoctorStatus::Ok),
        "a valid workspace shadow must not make its global peer faulty: {rows:?}"
    );
}

#[test]
fn doctor_reports_skill_directories_without_skill_markdown() {
    let root = tempdir().expect("catalog tempdir");
    write_skill(root.path(), "orbit", "healthy skill");

    let reference_residue = root.path().join("orbit-search");
    fs::create_dir_all(reference_residue.join("references")).expect("create reference residue");
    fs::write(
        reference_residue.join("references/search.md"),
        "retired reference\n",
    )
    .expect("write retired reference");
    let empty_residue = root.path().join("orbit-task");
    fs::create_dir(&empty_residue).expect("create empty residue");

    let catalog = SkillCatalog::new(root.path().to_path_buf());
    let rows = catalog.doctor().expect("doctor catalog");
    assert_eq!(rows.len(), 3, "every immediate skill directory is scanned");
    assert_eq!(
        rows.iter()
            .filter(|row| row.status == SkillCatalogDoctorStatus::Ok)
            .count(),
        1
    );
    for residue in [&reference_residue, &empty_residue] {
        let id = residue
            .file_name()
            .and_then(|name| name.to_str())
            .expect("residue id");
        let row = rows
            .iter()
            .find(|row| row.skill_id == id)
            .expect("residue row");
        assert_eq!(row.status, SkillCatalogDoctorStatus::Warning);
        assert!(row.message.contains("missing SKILL.md"), "{}", row.message);
        assert!(
            row.message.contains(residue.to_string_lossy().as_ref()),
            "the finding names the residue directory: {}",
            row.message
        );
    }

    assert_eq!(
        catalog.list().expect("list healthy skills").len(),
        1,
        "residue is diagnosed but never loaded"
    );
}

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
