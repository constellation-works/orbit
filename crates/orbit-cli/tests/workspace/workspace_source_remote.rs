#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Binary-level coverage for explicit workspace source-remote rebinding.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

const OLD_REMOTE: &str = "git@github.com:example/orbit.git";
const NEW_REMOTE: &str = "ssh://github.com/example/orbit-renamed.git";

#[test]
fn force_init_binds_first_origin_and_preserves_task_partition_and_publication() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    run_git(repo.path(), &["init"]);
    initialize_workspace(repo.path(), home.path(), "first-origin-owner");
    let registry_path = home.path().join(".orbit/workspaces.json");
    let initial_registry = read_json(&registry_path);
    assert!(initial_registry["workspaces"][0]["git_remote"].is_null());
    let initial_identity = fs::read(repo.path().join(".orbit/config.yaml")).expect("identity");
    let task = run_json(
        repo.path(),
        home.path(),
        &[
            "task",
            "add",
            "--title",
            "Preserved across first origin binding",
            "--description",
            "Disposable first-origin fixture",
            "--acceptance-criteria",
            "The original task partition remains readable",
            "--complexity",
            "low",
            "--model",
            "codex",
            "--json",
        ],
    );
    let task_id = task["id"].as_str().expect("created task id");
    let original_task = run_json(
        repo.path(),
        home.path(),
        &["task", "show", task_id, "--json"],
    );
    let partition = home.path().join(".orbit/tasks/workspaces/ws_orbit");
    assert!(partition.join(task_id).is_dir());

    run_git(repo.path(), &["remote", "add", "origin", OLD_REMOTE]);
    orbit(repo.path(), home.path())
        .args(["workspace", "init", "--name", "orbit"])
        .assert()
        .failure();
    let before_binding = fs::read(&registry_path).expect("registry before binding");
    for args in [
        vec![
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            OLD_REMOTE,
        ],
        vec![
            "workspace",
            "publication",
            "bind",
            "--remote",
            "git@github.com:example/task-publication.git",
            "--publication-id",
            "pub_first_origin",
        ],
    ] {
        orbit(repo.path(), home.path())
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "workspace init --name orbit --force",
            ));
        assert_eq!(
            fs::read(&registry_path).expect("unchanged registry"),
            before_binding
        );
    }

    force_init(repo.path(), home.path());
    let bound_registry = read_json(&registry_path);
    let mut expected_workspace = initial_registry["workspaces"][0].clone();
    expected_workspace["git_remote"] = Value::String(OLD_REMOTE.to_string());
    expected_workspace["updated_at"] = bound_registry["workspaces"][0]["updated_at"].clone();
    assert_eq!(bound_registry["workspaces"][0], expected_workspace);
    assert_eq!(bound_registry["checkouts"], initial_registry["checkouts"]);
    assert_eq!(
        fs::read(repo.path().join(".orbit/config.yaml")).expect("identity"),
        initial_identity
    );
    let source = run_json(
        repo.path(),
        home.path(),
        &["workspace", "source-remote", "show", "--json"],
    );
    assert_eq!(source["workspace_id"], "ws_orbit");
    assert_eq!(source["remote"], OLD_REMOTE);
    assert_eq!(source["repository_identity"], "github.com/example/orbit");
    let publication = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "publication",
            "bind",
            "--remote",
            "git@github.com:example/task-publication.git",
            "--publication-id",
            "pub_first_origin",
            "--json",
        ],
    );
    assert_eq!(publication["source_repository_fingerprint"], OLD_REMOTE);
    let publication = run_json(
        repo.path(),
        home.path(),
        &["workspace", "publication", "show", "--json"],
    );
    let published_registry = read_json(&registry_path);

    // Exact retries, equivalent origins, and an absent origin must all preserve
    // the stored spelling, publication lineage and original task partition.
    for origin in [
        Some(OLD_REMOTE),
        Some("https://github.com/Example/Orbit.git"),
        None,
    ] {
        if let Some(origin) = origin {
            run_git(repo.path(), &["remote", "set-url", "origin", origin]);
        } else {
            run_git(repo.path(), &["remote", "remove", "origin"]);
        }
        force_init(repo.path(), home.path());
        let mut retried_registry = read_json(&registry_path);
        retried_registry["workspaces"][0]["updated_at"] =
            published_registry["workspaces"][0]["updated_at"].clone();
        assert_eq!(retried_registry, published_registry);
        assert_eq!(
            run_json(
                repo.path(),
                home.path(),
                &["task", "show", task_id, "--json"]
            ),
            original_task
        );
        assert!(partition.join(task_id).is_dir());
        assert_eq!(
            run_json(
                repo.path(),
                home.path(),
                &["workspace", "publication", "show", "--json"]
            ),
            publication
        );
    }

    run_git(repo.path(), &["remote", "add", "origin", NEW_REMOTE]);
    assert_force_init_refused_without_writes(repo.path(), home.path(), "source-remote rebind");
    // An explicit rebind still refuses to rewrite an existing publication lineage.
    orbit(repo.path(), home.path())
        .args([
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refusing to rewrite publication lineage",
        ));
    assert_eq!(
        read_json(&registry_path)["publication_bindings"],
        published_registry["publication_bindings"]
    );
    assert_eq!(
        run_json(
            repo.path(),
            home.path(),
            &["task", "show", task_id, "--json"]
        ),
        original_task
    );
}

#[test]
fn force_init_recovers_identity_with_an_unchanged_non_portable_origin() {
    for origin in [
        "/local/source.git",
        "file:///local/source.git",
        "https://operator:supersecret@github.com/example/orbit.git",
    ] {
        let home = tempdir().expect("home");
        let repo = tempdir().expect("repo");
        init_git_repo(repo.path(), origin);
        initialize_workspace(repo.path(), home.path(), "non-portable-origin-owner");
        let registry_path = home.path().join(".orbit/workspaces.json");
        let initial_registry = read_json(&registry_path);
        assert_eq!(initial_registry["workspaces"][0]["git_remote"], origin);
        let identity_path = repo.path().join(".orbit/config.yaml");
        let initial_identity: Value =
            serde_yaml::from_slice(&fs::read(&identity_path).expect("identity"))
                .expect("parse identity");

        for corrupt in [false, true] {
            if corrupt {
                fs::write(&identity_path, "workspace_id: [\n").expect("corrupt identity");
            } else {
                fs::remove_file(&identity_path).expect("remove identity");
            }
            orbit(repo.path(), home.path())
                .args([
                    "workspace",
                    "init",
                    "--name",
                    "orbit",
                    "--force",
                    "--base-branch",
                    "recovered-branch",
                    "--ship-mode",
                    "local",
                ])
                .assert()
                .success()
                .stdout(predicate::str::contains("supersecret").not())
                .stderr(predicate::str::contains("supersecret").not());
            let recovered_identity: Value =
                serde_yaml::from_slice(&fs::read(&identity_path).expect("recovered identity"))
                    .expect("parse recovered identity");
            assert_eq!(recovered_identity, initial_identity);
            let reconciled_registry = read_json(&registry_path);
            let mut expected_workspace = initial_registry["workspaces"][0].clone();
            expected_workspace["base_branch"] = Value::String("recovered-branch".to_string());
            expected_workspace["ship_mode"] = Value::String("local".to_string());
            expected_workspace["updated_at"] =
                reconciled_registry["workspaces"][0]["updated_at"].clone();
            assert_eq!(reconciled_registry["workspaces"][0], expected_workspace);
            assert_eq!(
                reconciled_registry["checkouts"],
                initial_registry["checkouts"]
            );
            assert_eq!(
                reconciled_registry["publication_bindings"],
                initial_registry["publication_bindings"]
            );
        }

        run_git(repo.path(), &["remote", "set-url", "origin", NEW_REMOTE]);
        assert_force_init_refused_without_writes(repo.path(), home.path(), "source-remote rebind");
    }
}

#[test]
fn force_init_rejects_invalid_origins_before_first_binding_and_on_retry() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    run_git(repo.path(), &["init"]);
    initialize_workspace(repo.path(), home.path(), "invalid-origin-owner");
    run_git(repo.path(), &["remote", "add", "origin", OLD_REMOTE]);

    for already_bound in [false, true] {
        if already_bound {
            run_git(repo.path(), &["remote", "set-url", "origin", OLD_REMOTE]);
            force_init(repo.path(), home.path());
        }
        for (origin, error) in [
            (
                "https://operator:supersecret@github.com/example/orbit.git",
                "must not contain credentials",
            ),
            ("/local/source.git", "portable Git remote"),
            ("origin", "Git remote"),
            ("https://github.com", "Git remote"),
        ] {
            run_git(repo.path(), &["remote", "set-url", "origin", origin]);
            assert_force_init_refused_without_writes(
                repo.path(),
                home.path(),
                if already_bound {
                    "source-remote rebind"
                } else {
                    error
                },
            );
        }
    }
}

#[test]
fn force_init_cannot_bind_first_origin_from_a_replica_or_foreign_owner() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    run_git(repo.path(), &["init"]);
    initialize_workspace(repo.path(), home.path(), "authority-origin-owner");
    let registry_path = home.path().join(".orbit/workspaces.json");
    let initial_registry = read_json(&registry_path);
    run_git(repo.path(), &["remote", "add", "origin", OLD_REMOTE]);

    assert_init_refused_without_writes(
        repo.path(),
        home.path(),
        "refusing to rebind",
        &["--role", "replica", "--owner", "hm_fixture_remote"],
    );

    // These are valid registry facts for another owner's checkout, rather than
    // an inferred owner from the new origin.
    for replica in [true, false] {
        let mut registry = initial_registry.clone();
        registry["workspaces"][0]["owner_machine_id"] =
            Value::String("hm_fixture_remote".to_string());
        if replica {
            registry["checkouts"][0]["role"] = Value::String("replica".to_string());
            registry["checkouts"][0]["owner_machine_id"] =
                Value::String("hm_fixture_remote".to_string());
        }
        fs::write(
            &registry_path,
            serde_json::to_vec_pretty(&registry).expect("registry JSON"),
        )
        .expect("write foreign-owner fixture");
        assert_force_init_refused_without_writes(
            repo.path(),
            home.path(),
            if replica { "replica" } else { "logical owner" },
        );
    }
}

#[test]
fn owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    init_git_repo(repo.path(), OLD_REMOTE);
    initialize_workspace(repo.path(), home.path(), "remote-owner");

    let task = run_json(
        repo.path(),
        home.path(),
        &[
            "task",
            "add",
            "--title",
            "Preserved across source move",
            "--description",
            "Disposable source-remote fixture",
            "--acceptance-criteria",
            "The task remains readable",
            "--complexity",
            "low",
            "--model",
            "codex",
            "--json",
        ],
    );
    let task_id = task["id"].as_str().expect("created task id").to_string();

    let registry_path = home.path().join(".orbit/workspaces.json");
    let initial_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("initial registry"))
            .expect("parse initial registry");
    let initial_workspace = initial_registry["workspaces"][0].clone();
    let initial_checkouts = initial_registry["checkouts"].clone();

    let inspected = run_json(
        repo.path(),
        home.path(),
        &["workspace", "source-remote", "show", "--json"],
    );
    assert_eq!(inspected["remote"], OLD_REMOTE);
    assert_eq!(inspected["repository_identity"], "github.com/example/orbit");

    let before_dry_run = fs::read(&registry_path).expect("registry before dry run");
    let dry_run = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(dry_run["action"], "would_rebind");
    assert_eq!(dry_run["changed"], true);
    assert_eq!(dry_run["dry_run"], true);
    assert_eq!(
        fs::read(&registry_path).expect("registry after dry run"),
        before_dry_run
    );

    let applied = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
            "--json",
        ],
    );
    assert_eq!(applied["action"], "rebound");
    assert_eq!(
        applied["old"]["repository_identity"],
        "github.com/example/orbit"
    );
    assert_eq!(
        applied["new"]["repository_identity"],
        "github.com/example/orbit-renamed"
    );

    let rebound_registry: Value =
        serde_json::from_slice(&fs::read(&registry_path).expect("rebound registry"))
            .expect("parse rebound registry");
    assert_eq!(
        rebound_registry["workspaces"][0]["id"],
        initial_workspace["id"]
    );
    assert_eq!(
        rebound_registry["workspaces"][0]["owner_machine_id"],
        initial_workspace["owner_machine_id"]
    );
    assert_eq!(rebound_registry["checkouts"], initial_checkouts);
    assert_eq!(rebound_registry["workspaces"][0]["git_remote"], NEW_REMOTE);
    let preserved_task = run_json(
        repo.path(),
        home.path(),
        &["task", "show", &task_id, "--json"],
    );
    assert_eq!(preserved_task["id"], task_id);
    assert_eq!(preserved_task["title"], "Preserved across source move");

    let before_retry = fs::read(&registry_path).expect("registry before retry");
    let retry = run_json(
        repo.path(),
        home.path(),
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            "https://github.com/Example/Orbit-Renamed.git",
            "--json",
        ],
    );
    assert_eq!(retry["action"], "unchanged");
    assert_eq!(retry["changed"], false);
    assert_eq!(
        fs::read(&registry_path).expect("registry after retry"),
        before_retry
    );

    let verified = run_json(
        repo.path(),
        home.path(),
        &["workspace", "source-remote", "show", "--json"],
    );
    assert_eq!(verified["remote"], NEW_REMOTE);
}

#[test]
fn credential_bearing_rebind_is_redacted_and_does_not_mutate() {
    let home = tempdir().expect("home");
    let repo = tempdir().expect("repo");
    init_git_repo(repo.path(), OLD_REMOTE);
    initialize_workspace(repo.path(), home.path(), "remote-owner");
    let registry_path = home.path().join(".orbit/workspaces.json");
    let before = fs::read(&registry_path).expect("registry before rejection");

    orbit(repo.path(), home.path())
        .args([
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            "https://operator:supersecret@github.com/example/orbit.git",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not contain credentials"))
        .stderr(predicate::str::contains("supersecret").not());

    assert_eq!(
        fs::read(&registry_path).expect("registry after rejection"),
        before
    );
}

#[test]
fn source_remote_help_names_inspection_dry_run_and_rebind() {
    let home = tempdir().expect("home");
    let cwd = tempdir().expect("cwd");
    run_git(cwd.path(), &["init"]);
    orbit(cwd.path(), home.path())
        .args(["workspace", "source-remote", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("rebind"))
        .stdout(predicate::str::contains("verify"))
        .stdout(predicate::str::contains("roll back"))
        .stdout(predicate::str::contains("publication bindings"));
    orbit(cwd.path(), home.path())
        .args(["workspace", "source-remote", "rebind", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("Credentials"));
    orbit(cwd.path(), home.path())
        .args(["workspace", "init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("first source identity"))
        .stdout(predicate::str::contains("source-remote"));
}

fn force_init(repo: &Path, home: &Path) {
    orbit(repo, home)
        .args(["workspace", "init", "--name", "orbit", "--force"])
        .assert()
        .success();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("read JSON")).expect("parse JSON")
}

fn assert_force_init_refused_without_writes(repo: &Path, home: &Path, error: &str) {
    assert_init_refused_without_writes(repo, home, error, &[]);
}

fn assert_init_refused_without_writes(repo: &Path, home: &Path, error: &str, extra_args: &[&str]) {
    // If rejection ran bootstrap first, it would restore the managed ignore
    // entry; preserving this operator edit proves rejection precedes that write.
    fs::write(repo.join(".gitignore"), "operator-file\n").expect("operator ignore edit");
    let paths = [
        home.join(".orbit/workspaces.json"),
        repo.join(".orbit/config.yaml"),
        repo.join(".gitignore"),
    ];
    let before: Vec<_> = paths
        .iter()
        .map(|path| fs::read(path).expect("before refusal"))
        .collect();
    orbit(repo, home)
        .args([
            "workspace",
            "init",
            "--name",
            "orbit",
            "--force",
            "--base-branch",
            "changed-branch",
            "--ship-mode",
            "local",
        ])
        .args(extra_args)
        .assert()
        .failure()
        .stderr(predicate::str::contains(error))
        .stderr(predicate::str::contains("supersecret").not());
    for (path, before) in paths.iter().zip(before) {
        assert_eq!(
            fs::read(path).expect("after refusal"),
            before,
            "refusal changed {}",
            path.display()
        );
    }
}

fn initialize_workspace(repo: &Path, home: &Path, machine_name: &str) {
    orbit(repo, home)
        .args([
            "init",
            "--non-interactive",
            "--skip-host-prerequisites",
            "--machine-name",
            machine_name,
            "--task-prefix",
            "TST",
        ])
        .assert()
        .success();
    orbit(repo, home)
        .args([
            "workspace",
            "init",
            "--name",
            "orbit",
            "--base-branch",
            "agent-main",
        ])
        .assert()
        .success();
}

fn run_json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let output = orbit(cwd, home).args(args).assert().success();
    serde_json::from_slice(&output.get_output().stdout).expect("parse command JSON")
}

fn orbit(cwd: &Path, home: &Path) -> assert_cmd::Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("ORBIT_HOME");
    command
}

fn init_git_repo(repo: &Path, remote: &str) {
    run_git(repo, &["init"]);
    run_git(repo, &["remote", "add", "origin", remote]);
}

fn run_git(cwd: &Path, args: &[&str]) {
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
}
