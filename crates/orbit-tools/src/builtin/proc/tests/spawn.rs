use super::enforce_program_allowlist;
use crate::ToolContext;
use orbit_common::OrbitError;

#[cfg(unix)]
#[test]
fn disallow_mode_checks_canonical_symlink_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("sudo");
    std::fs::write(&target, b"fixture").expect("target");
    let link = dir.path().join("alias");
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    let ctx = ToolContext {
        proc_disallowed_programs: Some(vec!["sudo".to_string()]),
        ..Default::default()
    };
    let error = enforce_program_allowlist(&ctx, "proc.spawn", link.to_str().expect("utf8 path"))
        .expect_err("alias to listed executable must be denied");
    assert!(matches!(error, OrbitError::PolicyDenied(_)));
}
