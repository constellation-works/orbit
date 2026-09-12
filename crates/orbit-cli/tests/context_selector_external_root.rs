#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! ORB-12222: in the shared external-root layout, a call whose cwd is not
//! inside the registered checkout must still validate context selectors
//! against that checkout — never against `parent(<orbit-root>)`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output};

use assert_cmd::Command as AssertCommand;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

/// One registered checkout sharing an Orbit data directory outside the repo.
struct Workspace {
    temp: TempDir,
    home: PathBuf,
    orbit_root: PathBuf,
    repo: PathBuf,
    outside: PathBuf,
}

impl Workspace {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        // Nested so `dir:root` names the data directory under its parent —
        // the selector the pre-fix guard accepted from any cwd.
        let orbit_root = temp.path().join("orbit-data").join("root");
        let repo = temp.path().join("repo");
        let outside = temp.path().join("outside");
        for directory in [&home, &outside] {
            fs::create_dir_all(directory).expect("create fixture directory");
        }
        fs::create_dir_all(repo.join("src")).expect("create repo src");

        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.name", "Orbit Test"]);
        run_git(&repo, &["config", "user.email", "orbit-test@example.com"]);
        run_git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write committed file");
        run_git(&repo, &["add", "src/main.rs"]);
        run_git(&repo, &["commit", "-m", "initial"]);

        let workspace = Self {
            temp,
            home,
            orbit_root,
            repo: fs::canonicalize(&repo).expect("canonicalize repo"),
            outside: fs::canonicalize(&outside).expect("canonicalize outside cwd"),
        };
        let rooted_init = workspace.rooted(&[
            "init",
            "--non-interactive",
            "--host-name",
            "external-root-host",
            "--task-prefix",
            "EXR",
        ]);
        workspace.run_success(&workspace.repo, &string_refs(&rooted_init));
        let rooted_ws = workspace.rooted(&["workspace", "init", "--name", "repo"]);
        workspace.run_success(&workspace.repo, &string_refs(&rooted_ws));
        workspace.assert_bound_from_checkout();
        workspace
    }

    fn orbit_root(&self) -> PathBuf {
        fs::canonicalize(&self.orbit_root).expect("canonicalize orbit root")
    }

    fn rooted(&self, args: &[&str]) -> Vec<String> {
        let mut rooted = vec![
            "--root".to_string(),
            self.orbit_root
                .to_str()
                .expect("utf8 orbit root")
                .to_string(),
        ];
        rooted.extend(args.iter().map(|arg| (*arg).to_string()));
        rooted
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

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        let output = self.run_success(cwd, args);
        serde_json::from_slice(&output.stdout).expect("orbit json output")
    }

    fn rooted_json(&self, cwd: &Path, args: &[&str]) -> Value {
        let rooted = self.rooted(args);
        self.json(cwd, &string_refs(&rooted))
    }

    fn assert_bound_from_checkout(&self) {
        let shown = self.rooted_json(&self.repo, &["workspace", "show", "--format", "json"]);
        let checkout = &shown["checkout"];
        assert_eq!(
            checkout["repo_root"].as_str(),
            self.repo.to_str(),
            "fixture must bind the disposable checkout: {shown}"
        );
        assert_eq!(
            checkout["orbit_dir"].as_str(),
            self.orbit_root().to_str(),
            "fixture must bind the shared Orbit data directory: {shown}"
        );
        assert!(!self.repo.join(".orbit").exists());
    }

    fn assert_unbound_from_outside(&self) {
        let shown = self.rooted_json(&self.outside, &["workspace", "show", "--format", "json"]);
        assert_eq!(
            shown["registered"], false,
            "cwd outside the checkout must not bind a checkout: {shown}"
        );
        assert!(
            shown["checkout"].is_null(),
            "cwd outside the checkout must report no checkout: {shown}"
        );
    }

    fn stored_context_files(&self, task_id: &str) -> Vec<String> {
        let task = self.rooted_json(&self.repo, &["task", "show", task_id, "--json"]);
        task["context_files"]
            .as_array()
            .unwrap_or_else(|| panic!("context_files in {task}"))
            .iter()
            .map(|entry| entry.as_str().unwrap_or_default().to_string())
            .collect()
    }

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
        fs::write(worktree.join(new_file), "fn brand_new() {}\n")
            .expect("write worktree-only file");
        worktree
    }
}

fn string_refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

#[test]
fn outside_cwd_validates_against_the_stored_checkout_not_the_orbit_root_parent() {
    let workspace = Workspace::init();
    workspace.assert_unbound_from_outside();

    let accepted = workspace.rooted(&[
        "task",
        "add",
        "--title",
        "Real selector from outside",
        "--description",
        "Must resolve against the stored checkout",
        "--complexity",
        "low",
        "--context",
        "file:src/main.rs",
        "--json",
    ]);
    let created = workspace.json(&workspace.outside, &string_refs(&accepted));
    let task_id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("task id in {created}"))
        .to_string();
    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec!["file:src/main.rs".to_string()],
        "`orbit task add` from outside the checkout must store the real selector"
    );

    let rejected_dir = workspace.rooted(&[
        "task",
        "add",
        "--title",
        "Bogus parent selector",
        "--description",
        "Must not resolve against parent(orbit-root)",
        "--complexity",
        "low",
        "--context",
        "dir:root",
    ]);
    let rejected = workspace.run_failure(&workspace.outside, &string_refs(&rejected_dir));
    assert!(
        rejected.contains("dir:root")
            && rejected.contains("does not resolve to an existing in-workspace target"),
        "`dir:root` names the Orbit data directory under its parent and must be refused: {rejected}"
    );

    let rejected_file = workspace.rooted(&[
        "task",
        "add",
        "--title",
        "Bogus data-dir file",
        "--description",
        "Must not resolve against the Orbit data directory",
        "--complexity",
        "low",
        "--context",
        "file:config.toml",
    ]);
    let rejected = workspace.run_failure(&workspace.outside, &string_refs(&rejected_file));
    assert!(
        rejected.contains("file:config.toml"),
        "a file that exists only under the Orbit data directory must be refused: {rejected}"
    );

    let tool_accept = json!({
        "title": "Tool real selector from outside",
        "description": "orbit.task.add must share the same guard root",
        "complexity": "low",
        "context_files": ["file:src/main.rs"],
    })
    .to_string();
    let tool_add = workspace.rooted(&["tool", "run", "orbit.task.add", "--input", &tool_accept]);
    let added = workspace.json(&workspace.outside, &string_refs(&tool_add));
    let tool_task_id = added["id"]
        .as_str()
        .unwrap_or_else(|| panic!("tool task id in {added}"))
        .to_string();
    assert_eq!(
        workspace.stored_context_files(&tool_task_id),
        vec!["file:src/main.rs".to_string()],
        "`orbit.task.add` from outside the checkout must store the real selector"
    );

    let tool_reject = json!({
        "id": tool_task_id,
        "context_files": ["dir:root"],
    })
    .to_string();
    let tool_update =
        workspace.rooted(&["tool", "run", "orbit.task.update", "--input", &tool_reject]);
    let rejected = workspace.run_failure(&workspace.outside, &string_refs(&tool_update));
    assert!(
        rejected.contains("dir:root"),
        "`orbit.task.update` must refuse a parent(orbit-root) selector: {rejected}"
    );
    assert_eq!(
        workspace.stored_context_files(&tool_task_id),
        vec!["file:src/main.rs".to_string()],
        "a refused declaration must leave the stored selectors untouched"
    );
}

#[test]
fn linked_worktree_of_an_external_root_checkout_can_declare_its_own_file() {
    let workspace = Workspace::init();
    let worktree = workspace.add_worktree_with_new_file(
        &workspace.temp.path().join("linked-worktree"),
        "orbit-external-root-wt",
        "src/brand_new.rs",
    );
    assert!(
        !workspace.repo.join("src/brand_new.rs").exists(),
        "the fixture file must exist only in the linked worktree"
    );

    let added = workspace.rooted(&[
        "task",
        "add",
        "--title",
        "Worktree file under a shared external root",
        "--description",
        "Caller-worktree fallback must see the stored checkout as repo_root",
        "--complexity",
        "low",
        "--context",
        "file:src/brand_new.rs",
        "--json",
    ]);
    let created = workspace.json(&worktree, &string_refs(&added));
    let task_id = created["id"]
        .as_str()
        .unwrap_or_else(|| panic!("task id in {created}"))
        .to_string();
    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec!["file:src/brand_new.rs".to_string()],
        "a linked worktree of a shared-root checkout must be able to declare its own file"
    );

    let tool_input = json!({
        "id": task_id,
        "context_files": ["file:src/main.rs", "file:src/brand_new.rs"],
    })
    .to_string();
    let tool_update =
        workspace.rooted(&["tool", "run", "orbit.task.update", "--input", &tool_input]);
    workspace.run_success(&worktree, &string_refs(&tool_update));
    assert_eq!(
        workspace.stored_context_files(&task_id),
        vec![
            "file:src/main.rs".to_string(),
            "file:src/brand_new.rs".to_string()
        ],
        "`orbit.task.update` must accept a worktree-local file alongside a shared one"
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
