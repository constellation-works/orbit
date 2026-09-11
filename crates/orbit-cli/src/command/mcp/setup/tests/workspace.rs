use tempfile::tempdir;

use super::super::workspace::resolve_workspace_layout_for_cwd;
use crate::tests::env_isolation::EnvGuard;
use orbit_core::OrbitError;

#[test]
fn resolve_workspace_layout_skips_global_home_orbit_during_walk_up() {
    let home = tempdir().expect("home tempdir");
    let global_orbit = home.path().join(".orbit");
    std::fs::create_dir_all(&global_orbit).expect("seed global orbit");
    let nested = home.path().join("uninitialized-project");
    std::fs::create_dir_all(&nested).expect("create nested cwd");
    let _env = EnvGuard::acquire().home(home.path());

    let err = resolve_workspace_layout_for_cwd(&nested, None)
        .expect_err("walk-up to $HOME/.orbit should fail");

    assert!(matches!(
        err,
        OrbitError::InvalidInput(message)
            if message.contains("not inside an initialized Orbit workspace")
    ));
}
