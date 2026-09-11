#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! ORB-12201: the operator-surface context-selector guard validates against the
//! checkout the caller stands in, not only the registered one. Managed job runs
//! execute inside a linked worktree, so a file such a run just created exists
//! only there — declaring it must not require `--allow-missing-context`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

/// A registered checkout with its own disposable Orbit home.
struct Workspace {
    temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Workspace {
    /// Initialize a Git repository, register it with Orbit, and confirm the
    /// child process really routes to these disposable paths before any task
    /// is written.
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(repo.join("src")).expect("create repo src");

        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.name", "Orbit Test"]);
        run_git(&repo, &["config", "user.email", "orbit-test@example.com"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("src/lib.rs"), "pub fn run() {}\n").expect("write committed file");
        run_git(&repo, &["add", "src/lib.rs"]);
        run_git(&repo, &["commit", "-m", "initial"]);

        let workspace = Self {
            temp,
            home,
            repo: fs::canonicalize(&repo).expect("canonicalize repo"),
        };
        workspace.run_success(&workspace.repo, &["workspace", "init"]);
        workspace.assert_routes_to_repo(&workspace.repo);
        workspace
    }

    fn temp_path(&self) -> &Path {
        self.temp.path()
    }

    fn run_success(&self, cwd: &Path, args: &[&str]) -> Output {
        let output = self.orbit(cwd, args).output().expect("run orbit");
        assert!(
            output.status.success(),
            "`orbit {}` in {} failed\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn run_failure(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.orbit(cwd, args).output().expect("run orbit");
        assert!(
            !output.status.success(),
            "`orbit {}` in {} unexpectedly succeeded\nstdout:\n{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout)
        );
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) -> AssertCommand {
        let mut command = cargo_bin_cmd!("orbit");
        // ORB-11300: scrub inherited managed-run authority before pinning this
        // fixture's own routing, so an ambient envelope cannot redirect writes.
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command.env_remove("LLVM_PROFILE_FILE");
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("ORBIT_AGENT_NAME", "claude")
            .env("ORBIT_AGENT_MODEL", "claude")
            .args(args);
        command
    }

    /// Child processes report physical paths, so compare against canonicalized
    /// fixture paths.
    fn assert_routes_to_repo(&self, cwd: &Path) {
        let shown = self.json(cwd, &["workspace", "show", "--format", "json"]);
        let checkout = &shown["checkout"];
        assert_eq!(
            checkout["repo_root"].as_str(),
            self.repo.to_str(),
            "fixture must bind the disposable checkout: {shown}"
        );
        assert_eq!(
            checkout["orbit_dir"].as_str(),
            self.repo.join(".orbit").to_str(),
            "fixture must bind the disposable Orbit directory: {shown}"
        );
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        let output = self.run_success(cwd, args);
        serde_json::from_slice(&output.stdout).expect("orbit json output")
    }

    /// File a task from the registered checkout and return its ID.
    fn author_task(&self, title: &str) -> String {
        let created = self.json(
            &self.repo,
            &[
                "task",
                "add",
                "--title",
                title,
                "--description",
                "Context selector guard fixture",
                "--complexity",
                "low",
                "--context",
                "file:src/lib.rs",
                "--json",
            ],
        );
        created["id"]
            .as_str()
            .unwrap_or_else(|| panic!("task id in {created}"))
            .to_string()
    }

    fn stored_context_files(&self, task_id: &str) -> Vec<String> {
        let task = self.json(&self.repo, &["task", "show", task_id, "--json"]);
        task["context_files"]
            .as_array()
            .unwrap_or_else(|| panic!("context_files in {task}"))
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Add a linked worktree at `path` holding a file that exists nowhere
    /// else, and return the worktree's canonical path.
    fn add_worktree_with_new_file(&self, path: &Path, branch: &str, new_file: &str) -> PathBuf {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create worktree parent");
        }
        run_git(
            &self.repo,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().expect("utf8 worktree path"),
            ],
        );
        let worktree = fs::canonicalize(path).expect("canonicalize linked worktree");
        fs::write(worktree.join(new_file), "pub fn brand_new() {}\n")
            .expect("write worktree-only file");
        worktree
    }
}

#[test]
fn linked_worktree_caller_can_declare_a_file_that_exists_only_there() {
    let workspace = Workspace::init();
    let task_id = workspace.author_task("Worktree context selector");
    let worktree = workspace.add_worktree_with_new_file(
        &workspace.temp_path().join("linked-worktree"),
        "orbit-context-selector",
        "src/brand_new.rs",
    );

    // The worktree binds the registered checkout, which is exactly why the
    // guard used to reject its own new files.
    workspace.assert_routes_to_repo(&worktree);
    assert!(
        !workspace.repo.join("src/brand_new.rs").exists(),
        "the fixture file must exist only in the linked worktree"
    );

    workspace.run_success(
        &worktree,
        &[
            "task",
            "update",
            &task_id,
            "--context",
            "file:src/brand_new.rs",
            "--json",
        ],
    );
    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec!["file:src/brand_new.rs".to_string()],
        "`orbit task update --context` must accept a worktree-local file"
    );

    let tool_input = json!({
        "id": task_id,
        "context_files": ["file:src/lib.rs", "file:src/brand_new.rs"],
    })
    .to_string();
    workspace.run_success(
        &worktree,
        &["tool", "run", "orbit.task.update", "--input", &tool_input],
    );
    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec![
            "file:src/lib.rs".to_string(),
            "file:src/brand_new.rs".to_string()
        ],
        "`orbit.task.update` must accept a worktree-local file alongside a shared one"
    );

    // The guard is relaxed for the caller's own checkout only: the same
    // selector from the registered checkout still names nothing.
    let rejected = workspace.run_failure(
        &workspace.repo,
        &[
            "task",
            "update",
            &task_id,
            "--context",
            "file:src/brand_new.rs",
        ],
    );
    assert!(
        rejected.contains("file:src/brand_new.rs"),
        "a caller outside the worktree must still be refused: {rejected}"
    );
}

#[test]
fn selector_missing_from_both_checkouts_is_rejected_from_a_worktree() {
    let workspace = Workspace::init();
    let task_id = workspace.author_task("Worktree context selector typo");
    let worktree = workspace.add_worktree_with_new_file(
        &workspace.temp_path().join("typo-worktree"),
        "orbit-context-typo",
        "src/brand_new.rs",
    );

    let rejected = workspace.run_failure(
        &worktree,
        &["task", "update", &task_id, "--context", "file:src/typo.rs"],
    );
    assert!(
        rejected.contains("file:src/typo.rs")
            && rejected.contains("does not resolve to an existing in-workspace target"),
        "`orbit task update --context` must still reject a dead selector: {rejected}"
    );

    let tool_input = json!({
        "id": task_id,
        "context_files": ["file:src/typo.rs"],
    })
    .to_string();
    let rejected = workspace.run_failure(
        &worktree,
        &["tool", "run", "orbit.task.update", "--input", &tool_input],
    );
    assert!(
        rejected.contains("file:src/typo.rs"),
        "`orbit.task.update` must still reject a dead selector: {rejected}"
    );

    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec!["file:src/lib.rs".to_string()],
        "a refused declaration must leave the stored selectors untouched"
    );
}

/// Managed job-run worktrees live under `<repo>/.orbit/state/worktrees/**`, so
/// they sit inside the registered checkout's directory tree while being
/// separate checkouts: a path-prefix test cannot tell them apart.
#[test]
fn worktree_nested_under_the_registered_checkout_can_declare_its_own_file() {
    let workspace = Workspace::init();
    let task_id = workspace.author_task("Nested worktree context selector");
    let worktree = workspace.add_worktree_with_new_file(
        &workspace.repo.join(".orbit/state/worktrees/jrun-fixture"),
        "orbit-context-nested",
        "src/brand_new.rs",
    );

    workspace.run_success(
        &worktree,
        &[
            "task",
            "update",
            &task_id,
            "--context",
            "file:src/brand_new.rs",
            "--json",
        ],
    );

    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec!["file:src/brand_new.rs".to_string()],
        "a job-run-shaped nested worktree must be able to declare its own file"
    );
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed\nstdout:\n{}\nstderr:\n{}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
