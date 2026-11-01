//! `orbit workspace sync --all` through the built binary: every workspace
//! registered on the host converges, one workspace failing or being held does
//! not stop the rest, and `--check` writes nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::{TempDir, tempdir};

use crate::git_repo;

/// A shipped auto-task every initialized workspace carries; removing it gives
/// `sync` something to recreate.
const SHIPPED_AUTO_TASK: &str = "auto_tasks/code-review.yaml";

struct Host {
    _temp: TempDir,
    home: PathBuf,
}

impl Host {
    fn new() -> Self {
        let temp = tempdir().expect("host tempdir");
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".orbit")).expect("create global root");
        std::fs::write(
            home.join(".orbit/config.toml"),
            "[machine]\nid = \"hm_sync_all\"\nname = \"sync-all-machine\"\ntask_prefix = \"ORB\"\n",
        )
        .expect("write machine identity");
        Self { _temp: temp, home }
    }

    fn orbit(&self, cwd: &Path) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env_remove("ORBIT_HOME");
        command.timeout(test_env::FIXTURE_STEP_DEADLINE);
        command
    }

    /// Register a workspace and delete one shipped auto-task from it, so a
    /// sync has a change to apply there.
    fn register_stale_workspace(&self, name: &str) -> PathBuf {
        let repo = self.home.join(name);
        git_repo::init(&repo);
        self.orbit(&repo)
            .args(["workspace", "init", "--name", name])
            .assert()
            .success();
        std::fs::remove_file(repo.join(".orbit").join(SHIPPED_AUTO_TASK))
            .expect("remove a shipped auto-task");
        repo
    }

    /// Run `sync --all` from the home directory, which is no workspace.
    fn sync_all(&self, extra: &[&str]) -> std::process::Output {
        self.orbit(&self.home)
            .args(["workspace", "sync", "--all", "--json"])
            .args(extra)
            .output()
            .expect("run workspace sync --all")
    }

    fn workspace_id(&self, repo: &Path) -> String {
        let config: Value =
            serde_yaml::from_slice(&std::fs::read(repo.join(".orbit/config.yaml")).unwrap())
                .unwrap();
        config["workspace_id"].as_str().unwrap().to_owned()
    }

    /// Record a run of `state` for the workspace in the host store.
    fn insert_run(&self, repo: &Path, run_id: &str, state: &str) {
        Connection::open(self.home.join(".orbit/orbit.db"))
            .expect("open host store")
            .execute(
                "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,created_at,pid) VALUES (?1,?2,'fixture',1,?3,'{}','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',?4)",
                params![run_id, self.workspace_id(repo), state, std::process::id()],
            )
            .expect("insert fixture run");
    }

    /// Record a run of `state` with an explicit owner pid and an old
    /// `started_at`, so the running-owner startup grace has passed.
    fn insert_run_with_owner(&self, repo: &Path, run_id: &str, state: &str, pid: u32) {
        Connection::open(self.home.join(".orbit/orbit.db"))
            .expect("open host store")
            .execute(
                "INSERT INTO job_runs (run_id,workspace_id,job_id,attempt,state,input_json,scheduled_at,started_at,created_at,pid) VALUES (?1,?2,'fixture',1,?3,'{}','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z','2020-01-01T00:00:00Z',?4)",
                params![run_id, self.workspace_id(repo), state, pid],
            )
            .expect("insert fixture run with owner");
    }

    fn set_run_state(&self, run_id: &str, state: &str) {
        Connection::open(self.home.join(".orbit/orbit.db"))
            .expect("open host store")
            .execute(
                "UPDATE job_runs SET state = ?1 WHERE run_id = ?2",
                params![state, run_id],
            )
            .expect("update fixture run");
    }

    /// Paths whose bytes differ between two snapshots (added, removed or
    /// changed).
    fn changed_paths(
        before: &BTreeMap<PathBuf, Vec<u8>>,
        after: &BTreeMap<PathBuf, Vec<u8>>,
    ) -> Vec<PathBuf> {
        before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .cloned()
            .collect()
    }

    /// Every file under the host's Orbit root and the workspaces' data
    /// directories, except process bookkeeping.
    fn snapshot(&self, repos: &[&Path]) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut roots = vec![self.home.join(".orbit")];
        roots.extend(repos.iter().map(|repo| repo.join(".orbit")));
        for root in roots {
            collect(&root, &mut files);
        }
        files
    }
}

fn collect(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).expect("read snapshot dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        // The store's bytes move on any open, and every process records its
        // participation in the host generation.
        if name.starts_with("orbit.db") || name == ".generation-participants" {
            continue;
        }
        if path.is_dir() {
            collect(&path, files);
        } else if let Ok(bytes) = std::fs::read(&path) {
            files.insert(path, bytes);
        }
    }
}

fn records(output: &std::process::Output) -> BTreeMap<String, Value> {
    let doc: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse sync --all JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    doc["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .map(|record| {
            (
                record["workspace"].as_str().unwrap().to_owned(),
                record.clone(),
            )
        })
        .collect()
}

#[test]
fn sync_all_keeps_going_past_a_failed_workspace_and_exits_non_zero() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let broken = host.register_stale_workspace("broken");
    let gamma = host.register_stale_workspace("gamma");
    // A regular file where the auto-task catalog belongs cannot be converged.
    std::fs::remove_dir_all(broken.join(".orbit/auto_tasks")).unwrap();
    std::fs::write(broken.join(".orbit/auto_tasks"), "not a directory").unwrap();

    let output = host.sync_all(&[]);
    let by_name = records(&output);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(by_name["alpha"]["status"], "synced", "{by_name:?}");
    assert_eq!(by_name["gamma"]["status"], "synced", "{by_name:?}");
    assert_eq!(by_name["broken"]["status"], "failed", "{by_name:?}");
    assert!(
        by_name["broken"]["error"]
            .as_str()
            .is_some_and(|e| !e.is_empty()),
        "{by_name:?}"
    );
    for repo in [&alpha, &gamma] {
        assert!(
            repo.join(".orbit").join(SHIPPED_AUTO_TASK).is_file(),
            "{} was not converged",
            repo.display()
        );
    }

    let human = host
        .orbit(&host.home)
        .args(["workspace", "sync", "--all"])
        .output()
        .expect("run human sync --all");
    let text = String::from_utf8_lossy(&human.stdout);
    assert_eq!(human.status.code(), Some(1), "{human:?}");
    assert!(text.contains("alpha: already current"), "{text}");
    assert!(text.contains("broken: failed"), "{text}");
    assert!(text.contains("gamma: already current"), "{text}");
}

#[test]
fn sync_all_skips_a_missing_checkout_with_a_reason_without_failing() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let gone = host.register_stale_workspace("gone");
    let gamma = host.register_stale_workspace("gamma");
    std::fs::remove_dir_all(&gone).unwrap();

    let output = host.sync_all(&[]);
    let by_name = records(&output);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(by_name["gone"]["status"], "skipped", "{by_name:?}");
    assert!(
        by_name["gone"]["reason"]
            .as_str()
            .unwrap()
            .contains("missing"),
        "{by_name:?}"
    );
    assert_eq!(by_name["alpha"]["status"], "synced", "{by_name:?}");
    assert_eq!(by_name["gamma"]["status"], "synced", "{by_name:?}");
    assert!(alpha.join(".orbit").join(SHIPPED_AUTO_TASK).is_file());
    assert!(gamma.join(".orbit").join(SHIPPED_AUTO_TASK).is_file());
}

#[test]
fn sync_check_all_writes_nothing_and_reports_pending_per_workspace() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let beta = host.register_stale_workspace("beta");
    // Converge beta so one workspace is pending and the other is current.
    host.orbit(&beta)
        .args(["workspace", "sync"])
        .assert()
        .success();
    let before = host.snapshot(&[&alpha, &beta]);

    let output = host.sync_all(&["--check"]);
    let by_name = records(&output);

    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert_eq!(by_name["alpha"]["status"], "pending", "{by_name:?}");
    assert!(
        by_name["alpha"]["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change.as_str().unwrap().contains("auto_task created=1")),
        "{by_name:?}"
    );
    assert_eq!(by_name["beta"]["status"], "already_current", "{by_name:?}");
    assert!(!alpha.join(".orbit").join(SHIPPED_AUTO_TASK).exists());
    assert_eq!(
        Host::changed_paths(&before, &host.snapshot(&[&alpha, &beta])),
        Vec::<PathBuf>::new(),
        "--check wrote"
    );
}

#[test]
fn sync_all_cannot_be_combined_with_workspace() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let before = host.snapshot(&[&alpha]);

    host.orbit(&host.home)
        .args(["workspace", "sync", "--all", "--workspace", "alpha"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "cannot be combined with `--workspace`",
        ));

    assert!(!alpha.join(".orbit").join(SHIPPED_AUTO_TASK).exists());
    assert_eq!(
        Host::changed_paths(&before, &host.snapshot(&[&alpha])),
        Vec::<PathBuf>::new(),
        "a refused sync wrote"
    );
}

#[test]
fn sync_all_skips_a_workspace_a_live_run_holds_until_the_run_ends() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let busy = host.register_stale_workspace("busy");
    // Opening a runtime creates the host store's tables.
    host.orbit(&busy)
        .args(["workspace", "list"])
        .assert()
        .success();
    host.insert_run(&busy, "jrun-sync-all-live", "running");

    let output = host.sync_all(&[]);
    let by_name = records(&output);

    assert!(
        output.status.success(),
        "a skip is not a failure: {output:?}"
    );
    assert_eq!(by_name["alpha"]["status"], "synced", "{by_name:?}");
    assert_eq!(by_name["busy"]["status"], "skipped", "{by_name:?}");
    assert!(
        by_name["busy"]["reason"]
            .as_str()
            .unwrap()
            .contains("jrun-sync-all-live"),
        "{by_name:?}"
    );
    assert!(alpha.join(".orbit").join(SHIPPED_AUTO_TASK).is_file());
    assert!(
        !busy.join(".orbit").join(SHIPPED_AUTO_TASK).exists(),
        "a held workspace was rewritten"
    );

    // `--check` reads only, so it still reports the held workspace's pending
    // changes.
    let checked = records(&host.sync_all(&["--check"]));
    assert_eq!(checked["busy"]["status"], "pending", "{checked:?}");

    host.set_run_state("jrun-sync-all-live", "success");
    let by_name = records(&host.sync_all(&[]));
    assert_eq!(by_name["busy"]["status"], "synced", "{by_name:?}");
    assert!(busy.join(".orbit").join(SHIPPED_AUTO_TASK).is_file());
}

#[cfg(unix)]
#[test]
fn sync_all_converges_a_workspace_whose_running_run_owner_is_gone() {
    let host = Host::new();
    let alpha = host.register_stale_workspace("alpha");
    let orphan = host.register_stale_workspace("orphan");
    // Opening a runtime creates the host store's tables.
    host.orbit(&orphan)
        .args(["workspace", "list"])
        .assert()
        .success();
    // A pid no OS assigns, with no start-time token and an old start:
    // the owner is conclusively missing past the startup grace.
    host.insert_run_with_owner(
        &orphan,
        "jrun-sync-all-dead-owner",
        "running",
        i32::MAX as u32,
    );

    let output = host.sync_all(&[]);
    let by_name = records(&output);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(by_name["alpha"]["status"], "synced", "{by_name:?}");
    assert_eq!(by_name["orphan"]["status"], "synced", "{by_name:?}");
    assert!(
        by_name["orphan"].get("reason").is_none_or(Value::is_null),
        "dead-owner run reported as holding the workspace: {by_name:?}"
    );
    assert!(alpha.join(".orbit").join(SHIPPED_AUTO_TASK).is_file());
    assert!(
        orphan.join(".orbit").join(SHIPPED_AUTO_TASK).is_file(),
        "workspace held by a dead-owner run was not converged"
    );
}
