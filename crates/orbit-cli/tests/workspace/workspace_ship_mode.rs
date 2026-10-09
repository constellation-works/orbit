//! A PR-mode workspace whose only remote is a local bare repository: the
//! built binary refuses to ship before any run or worktree exists, `doctor`
//! names the fix, and `workspace ship-mode` rebinds delivery in the registry
//! alone.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

const LOCAL_TAG: &str = "delivery:task_local_pipeline";
const REBIND: &str = "orbit workspace ship-mode local";

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    repo: PathBuf,
    bare: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().expect("fixture root");
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        let bare = root.path().join("constellation.git");
        fs::create_dir_all(&home).expect("home");
        crate::git_repo::init(&repo);
        for args in [
            &["config", "user.name", "Orbit Test"][..],
            &["config", "user.email", "orbit-test@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&repo, args);
        }
        fs::write(repo.join("README.md"), "fixture\n").expect("readme");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-m", "seed"]);
        git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&repo, &["push", "origin", "main"]);
        let fixture = Self {
            _root: root,
            home,
            repo,
            bare,
        };
        fixture.success(&[
            "init",
            "--non-interactive",
            "--skip-host-prerequisites",
            "--machine-name",
            "forgeless-owner",
            "--task-prefix",
            "TST",
        ]);
        fixture.success(&[
            "workspace",
            "init",
            "--name",
            "forgeless",
            "--ship-mode",
            "pr",
        ]);
        // Admission here tests forge routing, independently of the test host.
        fixture.success(&[
            "config",
            "set",
            "--global",
            "workflow.resource_throttle.enabled",
            "false",
        ]);
        fixture
    }

    fn orbit(&self) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_HOME");
        command
    }

    fn success(&self, args: &[&str]) -> String {
        let output = self.orbit().args(args).assert().success();
        String::from_utf8(output.get_output().stdout.clone()).expect("utf-8 stdout")
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_str(&self.success(args)).expect("command JSON")
    }

    fn failure(&self, args: &[&str]) -> Value {
        let output = self.orbit().args(args).output().expect("spawn orbit");
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        serde_json::from_slice(&output.stderr).unwrap_or_else(|_| {
            panic!(
                "{args:?} stderr is not one JSON error: {}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn task(&self, title: &str, tags: &[&str]) -> String {
        let mut args = vec![
            "task",
            "add",
            "--title",
            title,
            "--description",
            "Forgeless delivery fixture",
            "--acceptance-criteria",
            "The task is observable",
            "--complexity",
            "medium",
            "--context",
            "file:README.md",
            "--status",
            "backlog",
            "--json",
        ];
        for tag in tags {
            args.extend(["--tag", tag]);
        }
        self.json(&args)["id"]
            .as_str()
            .expect("task id")
            .to_string()
    }

    /// `doctor` exits nonzero while unrelated rows (provider CLIs) fail in a
    /// scratch home; its JSON still lists every row.
    fn doctor_row(&self) -> Value {
        let output = self
            .orbit()
            .args(["doctor", "--json"])
            .output()
            .expect("doctor");
        let rows: Value = serde_json::from_slice(&output.stdout).expect("doctor JSON");
        rows.as_array()
            .expect("doctor rows")
            .iter()
            .find(|row| row["check"] == "forge-remote")
            .cloned()
            .unwrap_or_else(|| panic!("no forge-remote row: {rows}"))
    }

    fn readiness(&self, task: &str) -> Value {
        let readiness = self.json(&["run", "readiness", "--json"]);
        readiness["tasks"]
            .as_array()
            .expect("readiness tasks")
            .iter()
            .find(|entry| entry["task_id"] == task)
            .cloned()
            .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
    }

    fn registry(&self) -> Value {
        serde_json::from_slice(&fs::read(self.home.join(".orbit/workspaces.json")).unwrap())
            .expect("registry JSON")
    }

    /// Every config and managed-default file in both roots, by path.
    fn managed_files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for root in [self.home.join(".orbit"), self.repo.join(".orbit")] {
            collect_managed(&root, &root, &mut files);
        }
        assert!(
            files.keys().any(|path| path.ends_with("config.yaml")),
            "the snapshot covers the workspace identity: {:?}",
            files.keys()
        );
        files
    }
}

/// Config and managed defaults are the YAML, TOML and Markdown under a root;
/// the registry and runtime state (stores, logs, locks) are not.
fn collect_managed(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("dir entry").path();
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        if path.is_dir() {
            if !matches!(
                relative.to_str(),
                Some("state" | "tmp" | "logs" | "runs" | "tasks" | "frictions")
            ) {
                collect_managed(root, &path, files);
            }
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("yaml" | "yml" | "toml" | "md")
        ) {
            files.insert(path.clone(), fs::read(&path).expect("read managed file"));
        }
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let mut command = StdCommand::new("git");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        cwd.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 git output")
}

fn assert_names_remote_and_fixes(message: &str, bare: &Path) {
    for expected in [bare.to_str().unwrap(), REBIND, LOCAL_TAG] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
}

/// Ship refuses with a typed error before any run or worktree exists, the
/// drain still admits the tagged task, and `doctor` names the fix.
#[test]
fn pr_workspace_without_a_forge_remote_refuses_ship_before_any_worktree_or_crew() {
    let fixture = Fixture::new();
    let plain = fixture.task("Plain forgeless task", &[]);
    let local = fixture.task("Locally delivered task", &[LOCAL_TAG]);

    let refused = fixture.failure(&["run", "ship", &plain, "--json"]);
    assert_eq!(refused["code"], "pr_forge_remote_missing", "{refused}");
    assert_names_remote_and_fixes(refused["error"].as_str().unwrap(), &fixture.bare);
    let runs = fixture.json(&["run", "history", "--no-reconcile", "--json"]);
    assert_eq!(
        runs["runs"].as_array().map(Vec::len),
        Some(0),
        "no run may exist: {runs}"
    );
    let worktrees = git(&fixture.repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(
        worktrees.matches("worktree ").count(),
        1,
        "no worktree may exist: {worktrees}"
    );

    let held = fixture.readiness(&plain);
    assert_eq!(held["eligible"], false, "{held}");
    assert_eq!(held["reason"], "pr_forge_remote_missing", "{held}");
    let admitted = fixture.readiness(&local);
    assert_eq!(admitted["eligible"], true, "{admitted}");

    let row = fixture.doctor_row();
    assert_eq!(row["status"], "warning", "{row}");
    assert_names_remote_and_fixes(row["message"].as_str().unwrap(), &fixture.bare);
    assert!(
        row["remediation"].as_str().unwrap().contains(REBIND),
        "{row}"
    );
}

/// `workspace ship-mode` reads the registered mode with no argument and
/// rebinds only the registry's ship mode; config and managed defaults stay
/// byte-identical and the refusal lifts.
#[test]
fn ship_mode_rebinds_only_the_registry_and_lifts_the_forge_refusal() {
    let fixture = Fixture::new();
    let plain = fixture.task("Plain forgeless task", &[]);
    assert_eq!(fixture.success(&["workspace", "ship-mode"]).trim(), "pr");
    let registry = fixture.registry();
    let managed = fixture.managed_files();

    let rebound = fixture.json(&["workspace", "ship-mode", "local", "--json"]);
    assert_eq!(rebound["action"], "rebound", "{rebound}");
    assert_eq!(rebound["previous"], "pr", "{rebound}");
    assert_eq!(rebound["ship_mode"], "local", "{rebound}");

    let after = fixture.registry();
    let mut expected = registry.clone();
    expected["workspaces"][0]["ship_mode"] = Value::String("local".to_string());
    expected["workspaces"][0]["updated_at"] = after["workspaces"][0]["updated_at"].clone();
    assert_eq!(after, expected, "only the ship mode may change");
    assert_eq!(
        fixture.managed_files(),
        managed,
        "config and managed defaults stay byte-identical"
    );
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    assert_eq!(shown["workspace"]["ship_mode"], "local", "{shown}");
    assert_eq!(fixture.success(&["workspace", "ship-mode"]).trim(), "local");

    let unchanged = fixture.json(&["workspace", "ship-mode", "local", "--json"]);
    assert_eq!(unchanged["action"], "unchanged", "{unchanged}");
    assert_eq!(fixture.registry(), after, "a no-op rebind writes nothing");

    assert_eq!(fixture.doctor_row()["status"], "skipped");
    let entry = fixture.readiness(&plain);
    assert_ne!(entry["reason"], "pr_forge_remote_missing", "{entry}");
}
