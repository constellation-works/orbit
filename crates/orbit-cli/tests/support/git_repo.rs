//! An independent Git checkout boundary for disposable CLI fixtures.
//!
//! Root discovery asks Git where a checkout ends. Git walks past a plain
//! directory or an empty `.git` directory, so a fixture whose TMPDIR sits
//! inside another checkout (a managed worktree's `.orbit/tmp`) would resolve
//! that checkout's Orbit root and identity instead of its own. A fixture that
//! initializes or mutates a workspace makes its checkout a real repository
//! first. Bootstrap (`workspace init`) then stops at that repository.
//!
//! Ordinary lookup does not. It keeps walking until it finds an `.orbit`
//! directory, which inside a managed worktree is the enclosing checkout.
//! [`seal_lookup_boundary`] plants an empty config there so lookup stops on
//! the fixture and cannot inherit the ancestor's crew pool.

use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::test_env;
use tempfile::{TempDir, tempdir_in};

/// Build a fixture Git command without inherited repository or Orbit authority.
/// Apply deliberate fixture environment settings after calling this helper.
pub(crate) fn command() -> Command {
    let mut git = Command::new("git");
    test_env::clear_inherited_authority(|name| {
        git.env_remove(name);
    });
    git
}

/// Create `path` and initialize it as a Git repository on branch `main`,
/// whatever the host's `init.defaultBranch`, because `workspace init` records
/// the checked-out branch as the workspace base branch.
///
/// Git location variables outrank `current_dir`. A managed run can export
/// them, and leaving them set makes `git init` update the enclosing
/// repository instead of the fixture.
pub(crate) fn init(path: &Path) {
    std::fs::create_dir_all(path).expect("create fixture checkout");
    let output = command()
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

/// Make `path` a Git checkout whose ordinary lookup cannot climb to an
/// ancestor workspace.
///
/// The empty `config.toml` is an initialized Orbit root with no crew pool.
/// Walk-up stops at it, so a command such as `task list` answers from this
/// checkout. Callers that run `workspace init` use [`init`] alone: bootstrap
/// already stops at the Git root, and a pre-existing config would collide
/// with init.
///
/// Some test binaries include this module only for [`init`].
#[allow(dead_code)]
pub(crate) fn seal_lookup_boundary(path: &Path) {
    init(path);
    let orbit = path.join(".orbit");
    std::fs::create_dir_all(&orbit).expect("create fixture orbit dir");
    std::fs::write(orbit.join("config.toml"), "").expect("write fixture lookup boundary");
}

/// A disposable home and a work directory that is its own Git repository.
///
/// Root discovery stops at `work` even when the temp directory sits inside
/// another checkout, which is where a managed run points `TMPDIR`. An empty
/// `.git` directory is not that boundary: Git walks past it.
///
/// Not every test binary that includes this module builds a checkout.
#[allow(dead_code)]
pub(crate) struct WorkCheckout {
    pub(crate) temp: TempDir,
    pub(crate) home: PathBuf,
    pub(crate) work: PathBuf,
}

#[allow(dead_code)]
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
        seal_lookup_boundary(&work);
        Self { temp, home, work }
    }
}
