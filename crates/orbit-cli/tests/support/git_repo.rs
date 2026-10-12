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
    initialize(path, Some("main"));
}

fn initialize(path: &Path, branch: Option<&str>) {
    std::fs::create_dir_all(path).expect("create fixture checkout");
    let mut git = command();
    git.args(["init", "--quiet"]);
    if let Some(branch) = branch {
        git.arg(format!("--initial-branch={branch}"));
    }
    let output = git.current_dir(path).output().expect("spawn git init");
    assert!(
        output.status.success(),
        "git init failed in fixture checkout {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run Git inside a fixture, checking success and returning trimmed stdout.
/// Inherited repository and Orbit authority are scrubbed by [`command`].
pub(crate) fn run(path: &Path, args: &[&str]) -> String {
    let output = command()
        .args(args)
        .current_dir(path)
        .output()
        .expect("run fixture git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}:\nstdout:\n{}\nstderr:\n{}",
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

enum SeedCommit<'a> {
    Readme(&'a str),
    Empty(&'a str),
    None,
}

/// A fixture checkout on `main` with a committed README by default.
/// Branch, README, identity and commit options keep specialized fixtures
/// explicit at their call sites. Uncommitted repositories have no seed file.
/// Some test binaries include this module only for [`init`].
#[allow(dead_code)]
pub(crate) struct SeededRepo<'a> {
    path: &'a Path,
    branch: Option<&'a str>,
    commit: SeedCommit<'a>,
    identity: Option<(&'a str, &'a str)>,
}

#[allow(dead_code)]
impl<'a> SeededRepo<'a> {
    pub(crate) fn new(path: &'a Path) -> Self {
        Self {
            path,
            branch: Some("main"),
            commit: SeedCommit::Readme("# repo\n"),
            identity: None,
        }
    }

    /// Pin the initial branch independently of the host Git configuration.
    pub(crate) fn branch(mut self, branch: &'a str) -> Self {
        self.branch = Some(branch);
        self
    }

    /// Preserve plain `git init` semantics for fixtures that accept any branch.
    pub(crate) fn host_default_branch(mut self) -> Self {
        self.branch = None;
        self
    }

    /// Seed these README bytes in an `initial` commit.
    pub(crate) fn readme(mut self, contents: &'a str) -> Self {
        self.commit = SeedCommit::Readme(contents);
        self
    }

    /// Create an empty initial commit with this message and no seed file.
    pub(crate) fn empty_commit(mut self, message: &'a str) -> Self {
        self.commit = SeedCommit::Empty(message);
        self
    }

    /// Leave the branch unborn, without creating a README or a commit.
    pub(crate) fn no_commit(mut self) -> Self {
        self.commit = SeedCommit::None;
        self
    }

    /// Set a fixture-local identity, including for an uncommitted checkout.
    pub(crate) fn identity(mut self, name: &'a str, email: &'a str) -> Self {
        self.identity = Some((name, email));
        self
    }

    pub(crate) fn build(self) {
        initialize(self.path, self.branch);
        let identity = self.identity.or_else(|| {
            (!matches!(self.commit, SeedCommit::None))
                .then_some(("Orbit Test", "orbit-test@example.com"))
        });
        if let Some((name, email)) = identity {
            run(self.path, &["config", "user.name", name]);
            run(self.path, &["config", "user.email", email]);
        }
        match self.commit {
            SeedCommit::Readme(contents) => {
                run(self.path, &["config", "commit.gpgsign", "false"]);
                std::fs::write(self.path.join("README.md"), contents)
                    .expect("write fixture README");
                run(self.path, &["add", "README.md"]);
                run(self.path, &["commit", "--quiet", "-m", "initial"]);
            }
            SeedCommit::Empty(message) => {
                run(self.path, &["config", "commit.gpgsign", "false"]);
                run(
                    self.path,
                    &["commit", "--quiet", "--allow-empty", "-m", message],
                );
            }
            SeedCommit::None => {}
        }
    }
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
