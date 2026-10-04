//! An independent Git checkout boundary for disposable CLI fixtures.
//!
//! Root discovery asks Git where a checkout ends. Git walks past a plain
//! directory or an empty `.git` directory, so a fixture whose TMPDIR sits
//! inside another checkout (a managed worktree's `.orbit/tmp`) would resolve
//! that checkout's Orbit root and identity instead of its own. A fixture that
//! initializes or mutates a workspace makes its checkout a real repository
//! first.

use std::path::Path;
use std::process::Command;

/// Create `path` and initialize it as a Git repository on branch `main`,
/// whatever the host's `init.defaultBranch`, because `workspace init` records
/// the checked-out branch as the workspace base branch.
pub(crate) fn init(path: &Path) {
    std::fs::create_dir_all(path).expect("create fixture checkout");
    let output = Command::new("git")
        .args(["init", "--quiet", "--initial-branch=main"])
        .current_dir(path)
        .output()
        .expect("spawn git init");
    assert!(
        output.status.success(),
        "git init failed in fixture checkout {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}
