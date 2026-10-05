//! An independent Git checkout boundary for disposable CLI fixtures.
//!
//! Root discovery asks Git where a checkout ends. Git walks past a plain
//! directory or an empty `.git` directory, so a fixture whose TMPDIR sits
//! inside another checkout (a managed worktree's `.orbit/tmp`) would resolve
//! that checkout's Orbit root and identity instead of its own. A fixture that
//! initializes or mutates a workspace makes its checkout a real repository
//! first.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir_in};

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

/// A disposable home and a work directory that is its own Git repository.
///
/// Root discovery stops at `work` even when the temp directory sits inside
/// another checkout, which is where a managed run points `TMPDIR`. An empty
/// `.git` directory is not that boundary: Git walks past it.
pub(crate) struct WorkCheckout {
    pub(crate) temp: TempDir,
    pub(crate) home: PathBuf,
    pub(crate) work: PathBuf,
}

impl WorkCheckout {
    /// Place the fixture in the process temp directory (`TMPDIR` when set).
    pub(crate) fn new() -> Self {
        Self::new_in(&std::env::temp_dir())
    }

    /// Place the fixture under `parent`. Passing a directory inside a Git
    /// checkout is how a regression proves the fixture does not inherit that
    /// checkout's Orbit config.
    pub(crate) fn new_in(parent: &Path) -> Self {
        let temp = tempdir_in(parent).expect("fixture tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create fixture home");
        init(&work);
        Self { temp, home, work }
    }
}
