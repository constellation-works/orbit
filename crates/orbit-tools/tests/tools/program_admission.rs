use orbit_common::OrbitError;
use orbit_tools::{ToolContext, ToolRegistry};

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
    let error = registry()
        .execute(
            "proc.spawn",
            &ctx,
            serde_json::json!({"program": link.to_str().expect("utf8 path")}),
        )
        .expect_err("alias to listed executable must be denied");
    assert!(matches!(error, OrbitError::PolicyDenied(_)));
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
}
