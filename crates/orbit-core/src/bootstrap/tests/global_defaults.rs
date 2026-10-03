//! When a runtime open must reconcile the global managed defaults, and what it
//! must leave untouched when it does not.

use std::fs;
use std::path::PathBuf;

use tempfile::{TempDir, tempdir};

use crate::bootstrap::global_defaults::{
    global_defaults_are_current, record_global_defaults_reconciled,
};

struct Roots {
    _temp: TempDir,
    global: PathBuf,
}

fn roots() -> Roots {
    let temp = tempdir().expect("tempdir");
    let global = temp.path().join("global");
    Roots {
        _temp: temp,
        global,
    }
}

#[cfg(unix)]
#[test]
fn stamp_rejects_a_symlinked_resources_directory() {
    use std::os::unix::fs::symlink;

    let roots = roots();
    let outside = roots._temp.path().join("outside-resources");
    fs::create_dir_all(&roots.global).expect("create global root");
    fs::create_dir_all(&outside).expect("create outside resources directory");
    symlink(&outside, roots.global.join("resources")).expect("link resources outside root");

    assert!(
        !global_defaults_are_current(&roots.global),
        "a symlinked resources directory must not be read as a stamp location"
    );

    let error = record_global_defaults_reconciled(&roots.global)
        .expect_err("a symlinked resources directory must not receive a stamp");
    assert!(
        matches!(error, orbit_common::OrbitError::InvalidInput(_)),
        "the unsafe stamp location must be rejected"
    );
}
