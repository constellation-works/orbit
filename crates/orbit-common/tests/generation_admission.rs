#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(unix)]
#[test]
fn readonly_generation_record_refuses_takeover_without_descriptor_errors() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use orbit_common::fs::generation::GenerationGuard;
    use tempfile::tempdir;

    const RECORDED: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const CANDIDATE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    let root = tempdir().expect("generation root");
    drop(GenerationGuard::acquire(root.path(), RECORDED).expect("record generation"));

    let record = root.path().join(".generation.lock");
    let admission_record = root.path().join(".generation-admission.lock");
    let before = fs::read(&record).expect("read generation record");
    for path in [&record, &admission_record] {
        let mut permissions = fs::metadata(path).expect("record metadata").permissions();
        permissions.set_mode(0o444);
        fs::set_permissions(path, permissions).expect("freeze admission record");
    }

    let refusal = match GenerationGuard::acquire(root.path(), CANDIDATE) {
        Ok(_) => panic!("read-only generation record must refuse a takeover"),
        Err(error) => error.to_string(),
    };
    assert!(
        refusal.contains("cannot be written from here"),
        "refusal should identify the read-only record: {refusal}"
    );
    assert!(
        !refusal.contains("Bad file descriptor"),
        "read-only admission must not surface a descriptor error: {refusal}"
    );
    assert_eq!(fs::read(&record).expect("read unchanged record"), before);

    for path in [&record, &admission_record] {
        let mut permissions = fs::metadata(path).expect("record metadata").permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(path, permissions).expect("restore admission record");
    }
    drop(
        GenerationGuard::acquire(root.path(), CANDIDATE)
            .expect("writable generation record admits a quiescent takeover"),
    );
}
