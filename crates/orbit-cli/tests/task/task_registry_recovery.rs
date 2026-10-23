//! Recovery after the shared task registry is lost: every workspace's tasks
//! come back through reindex, no issued id is reissued, and the orphan
//! task-store repair (`orbit_cmd::task_store`) deletes no populated partition
//! it cannot prove dead.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_cmd::task_store::{inspect_task_store_partitions, remove_unclaimed_task_stores};
use orbit_cmd::task_store_partition_path;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    root: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let fixture = Self {
            home: temp.path().join("home"),
            root: temp.path().join("orbit-root"),
            alpha: temp.path().join("alpha"),
            // Beneath its own parent, so a case can make that parent unreadable.
            beta: temp.path().join("volume").join("beta"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).expect("home");
        crate::git_repo::init(&fixture.alpha);
        crate::git_repo::init(&fixture.beta);
        fixture
            .orbit(
                &fixture.alpha,
                &[
                    "init",
                    "--non-interactive",
                    "--machine-name",
                    "registry-recovery",
                    "--task-prefix",
                    "REC",
                ],
            )
            .success();
        for (repo, name) in [(&fixture.alpha, "alpha"), (&fixture.beta, "beta")] {
            fixture
                .orbit(repo, &["workspace", "init", "--name", name])
                .success();
        }
        fixture
    }

    fn orbit(&self, cwd: &Path, args: &[&str]) -> assert_cmd::assert::Assert {
        self.command(cwd, args).assert()
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .arg("--root")
            .arg(&self.root)
            .args(args);
        command
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        let output = self.orbit(cwd, args).success();
        serde_json::from_slice(&output.get_output().stdout).expect("orbit json output")
    }

    fn add(&self, cwd: &Path, title: &str) -> String {
        self.json(
            cwd,
            &[
                "task",
                "add",
                "--title",
                title,
                "--complexity",
                "low",
                "--json",
            ],
        )["id"]
            .as_str()
            .expect("task id")
            .to_string()
    }

    fn refused_add(&self, cwd: &Path) -> String {
        let output = self
            .orbit(
                cwd,
                &[
                    "task",
                    "add",
                    "--title",
                    "Must not allocate",
                    "--complexity",
                    "low",
                    "--json",
                ],
            )
            .failure();
        String::from_utf8_lossy(&output.get_output().stderr).into_owned()
    }

    fn lose_registry(&self) {
        let registry = self.root.join("tasks").join("index.sqlite");
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", registry.display()));
            if path.exists() {
                fs::remove_file(&path).expect("remove task registry");
            }
        }
    }
}

fn number(task_id: &str) -> u32 {
    task_id
        .rsplit_once('-')
        .and_then(|(_, digits)| digits.parse().ok())
        .expect("numeric task id")
}

#[test]
fn lost_registry_refuses_allocation_until_the_high_water_mark_is_reestablished() {
    let fixture = Fixture::new();
    let (alpha, beta) = (fixture.alpha.as_path(), fixture.beta.as_path());

    // Both workspaces draw ids from one allocator.
    let alpha_tasks = [
        fixture.add(alpha, "Alpha one"),
        fixture.add(alpha, "Alpha two"),
    ];
    let beta_tasks = [fixture.add(beta, "Beta one"), fixture.add(beta, "Beta two")];
    let deleted = fixture.add(beta, "Beta deleted");
    let issued: Vec<u32> = alpha_tasks
        .iter()
        .chain(&beta_tasks)
        .chain([&deleted])
        .map(|id| number(id))
        .collect();
    let surviving_max = alpha_tasks
        .iter()
        .chain(&beta_tasks)
        .map(|id| number(id))
        .max();
    assert!(
        surviving_max < Some(number(&deleted)),
        "the lost bundle must hold the high-water mark for this scenario"
    );

    // Lose the registry together with the newest bundle: its id was issued,
    // but no surviving bundle records it.
    fixture.lose_registry();
    fs::remove_dir_all(fixture.root.join("tasks/workspaces/ws_beta").join(&deleted))
        .expect("remove the newest bundle");

    // A registry recreated over existing partitions cannot know what was
    // issued, so it refuses rather than restarting the counter.
    for repo in [alpha, beta] {
        let stderr = fixture.refused_add(repo);
        assert!(
            stderr.contains("task id allocation refused"),
            "stderr: {stderr}"
        );
    }

    // Reindexing every workspace restores lookup in both, but surviving
    // bundles cannot prove the lost bundle's id was issued, so allocation
    // stays refused: the reindexed counter sits on that id.
    for repo in [alpha, beta] {
        fixture.orbit(repo, &["task", "reindex"]).success();
    }
    let stderr = fixture.refused_add(alpha);
    assert!(
        stderr.contains("task id allocation refused"),
        "reindex alone must not lift the refusal; stderr: {stderr}"
    );

    // The operator establishes the high-water mark; it only moves forward.
    let start = (number(&deleted) + 10).to_string();
    fixture
        .orbit(
            alpha,
            &[
                "workspace",
                "init",
                "--name",
                "alpha",
                "--force",
                "--task-id-start",
                &start,
            ],
        )
        .success();

    let new_alpha = fixture.add(alpha, "Alpha after recovery");
    let new_beta = fixture.add(beta, "Beta after recovery");
    for id in [&new_alpha, &new_beta] {
        assert!(
            !issued.contains(&number(id)) && number(id) > number(&deleted),
            "recovered allocation reissued an id: {id}"
        );
    }
    assert_ne!(new_alpha, new_beta);

    for (repo, workspace, ids) in [
        (alpha, "ws_alpha", alpha_tasks.iter().chain([&new_alpha])),
        (beta, "ws_beta", beta_tasks.iter().chain([&new_beta])),
    ] {
        for id in ids {
            let shown = fixture.json(repo, &["task", "show", id, "--json"]);
            assert_eq!(shown["workspace"]["id"], workspace, "task {id}");
        }
    }
}

/// [ORB-12131] A lost `tasks/index.sqlite` is recreated empty by the next
/// runtime bootstrap, which rebinds only the checkout the command runs from.
/// A live partition no catalog workspace claims then looks unclaimed, so the
/// repair must leave its bundles for `orbit task reindex` instead of deleting
/// them.
#[test]
fn losing_the_task_registry_leaves_populated_partitions_for_reindex() {
    if crate::isolated_test::run_in_child(
        "task_registry_recovery::losing_the_task_registry_leaves_populated_partitions_for_reindex",
    ) {
        return;
    }

    let fixture = Fixture::new();
    fixture.add(&fixture.alpha, "Alpha one");
    let beta_task = fixture.add(&fixture.beta, "Beta one");
    // Removing beta from the catalog keeps its partition, claimed only by the
    // checkout evidence the registry holds.
    fixture
        .command(&fixture.alpha, &["workspace", "remove", "beta"])
        .env("ORBIT_OPERATOR", "1")
        .assert()
        .success();

    fixture.lose_registry();
    // The bootstrap that recreates the registry rebinds only this checkout.
    fixture
        .orbit(&fixture.alpha, &["task", "reindex"])
        .success();

    let partitions = inspect_task_store_partitions(&fixture.root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert_eq!(partitions.scanned, 2);
    assert!(
        partitions.removable.is_empty(),
        "no populated partition may be offered for deletion: {:?}",
        partitions.removable
    );
    assert_eq!(
        partitions
            .unowned
            .iter()
            .map(|partition| partition.path.clone())
            .collect::<Vec<_>>(),
        vec![task_store_partition_path(&fixture.root, "ws_beta")]
    );

    let removed = remove_unclaimed_task_stores(&fixture.root).expect("run the repair");
    assert!(removed.is_empty(), "the repair removed {removed:?}");
    assert!(
        task_store_partition_path(&fixture.root, "ws_beta")
            .join(&beta_task)
            .is_dir(),
        "the unclaimed checkout's task bundle must survive the repair"
    );
}

/// [ORB-12143] Absence is the only stat answer that authorizes deleting
/// bundles. Here the checkout path resolves through a non-directory (`ENOTDIR`),
/// which is a failure to answer rather than a missing checkout.
#[test]
fn a_stat_failure_other_than_absence_keeps_a_populated_partition() {
    if crate::isolated_test::run_in_child(
        "task_registry_recovery::a_stat_failure_other_than_absence_keeps_a_populated_partition",
    ) {
        return;
    }

    let fixture = Fixture::new();
    let beta_task = fixture.add(&fixture.beta, "Beta one");

    let parent = fixture.beta.parent().expect("checkout parent");
    fs::remove_dir_all(parent).expect("remove checkout parent");
    fs::write(parent, b"not a directory").expect("replace checkout parent with a file");
    let error = fs::metadata(&fixture.beta).expect_err("stat must fail beneath a regular file");
    assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);

    let partitions = inspect_task_store_partitions(&fixture.root)
        .expect("inspect partitions")
        .expect("partitions directory exists");
    assert!(partitions.stale.is_empty(), "{:?}", partitions.stale);
    assert_eq!(partitions.unreachable.len(), 1, "{partitions:?}");

    let removed = remove_unclaimed_task_stores(&fixture.root).expect("run the repair");
    assert!(removed.is_empty(), "the repair removed {removed:?}");
    assert!(
        task_store_partition_path(&fixture.root, "ws_beta")
            .join(&beta_task)
            .is_dir()
    );
}
