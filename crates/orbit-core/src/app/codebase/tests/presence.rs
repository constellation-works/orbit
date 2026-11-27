use serde_json::json;

use super::super::test_mark_absent_row_with_lookup;

#[test]
fn permission_denied_lookup_does_not_claim_absence() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir())
        .expect("temporary repository");
    std::fs::create_dir_all(root.path().join("src")).expect("source directory");
    std::fs::write(root.path().join("src/denied.rs"), "present").expect("existing file");

    let mut detail = json!({"path": "src/denied.rs"});
    test_mark_absent_row_with_lookup(&mut detail, |path| {
        assert_eq!(path, "src/denied.rs");
        assert!(root.path().join(path).is_file(), "the denied path exists");
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });
    assert!(detail.get("absent_at_head").is_none(), "{detail}");

    let mut paths = json!({"paths": ["src/denied.rs", "src/gone.rs"]});
    test_mark_absent_row_with_lookup(&mut paths, |path| match path {
        "src/denied.rs" => {
            assert!(root.path().join(path).is_file(), "the denied path exists");
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }
        "src/gone.rs" => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        other => panic!("unexpected checkout path {other}"),
    });
    assert_eq!(paths["absent_paths_at_head"], json!(["src/gone.rs"]));
}
