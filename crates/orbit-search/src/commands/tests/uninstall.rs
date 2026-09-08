//! Unit tests for `uninstall` — sibling layout under commands/tests/.

use super::super::uninstall::{SemanticUninstallParams, run_with_paths};

use crate::CompanionPaths;

use tempfile::tempdir;

#[test]
fn uninstall_all_removes_companion_metadata_and_stale_temporary_files() {
    let temporary = tempdir().expect("temporary fixture");
    let paths = CompanionPaths::new(temporary.path().join("embed"));
    let companion = paths.companion_path();
    let companion_name = companion
        .file_name()
        .and_then(|name| name.to_str())
        .expect("platform companion file name");
    let manifest = companion.with_file_name(format!("{companion_name}.sha256"));
    let stale_temporary = paths.bin_dir.join(format!(".{companion_name}.tmp-123"));

    std::fs::create_dir_all(&paths.bin_dir).expect("create bin directory");
    std::fs::write(&companion, "binary").expect("write companion");
    std::fs::write(&manifest, "checksum").expect("write manifest");
    std::fs::write(&stale_temporary, "partial binary").expect("write stale temporary");

    let result = run_with_paths(
        &paths,
        SemanticUninstallParams {
            model: None,
            all: true,
        },
    )
    .expect("uninstall all");

    assert!(result.removed_companion);
    assert!(result.removed_companion_integrity);
    assert_eq!(
        result.removed_temporary_companions,
        vec![format!(".{companion_name}.tmp-123")]
    );
    let json = serde_json::to_value(&result).expect("serialize uninstall result");
    assert_eq!(json["removed_companion_integrity"], true);
    assert_eq!(
        json["removed_temporary_companions"],
        serde_json::json!([format!(".{companion_name}.tmp-123")])
    );
    assert!(
        !paths.bin_dir.exists(),
        "the empty bin directory should be removed"
    );
    assert!(
        !paths.root.exists(),
        "the empty embed root should be removed"
    );
}
