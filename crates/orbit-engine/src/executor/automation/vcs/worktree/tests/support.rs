use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Puts a read-only directory holding a file inside the worktree, the way an
/// aborted fixture leaves one, and keeps it out of the dirty-tree check.
#[cfg(unix)]
pub(super) fn leave_read_only_fixture(repo: &Path, worktree: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    fs::write(repo.join(".git/info/exclude"), ".orbit/tmp/\n").unwrap();
    let locked = worktree.join(".orbit/tmp/x/home/.orbit");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("state.db"), vec![7u8; 4096]).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    locked
}

/// Lets the tempdir delete the fixture if an assertion fails first.
#[cfg(unix)]
pub(super) struct Unlock(pub(super) PathBuf);

#[cfg(unix)]
impl Drop for Unlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

pub(super) fn listed_worktrees(repo: &Path) -> String {
    git(repo, &["worktree", "list", "--porcelain"])
}

pub(super) fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", "agent-main"]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(path.join("base.txt"), "base").unwrap();
    git(path, &["add", "base.txt"]);
    git(path, &["commit", "-m", "base"]);
}

pub(super) fn add_worktree(repo: &Path, path: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.args(args).current_dir(current_dir);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
