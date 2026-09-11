//! `orbit mcp init` / `orbit mcp remove` against an external Orbit data root.
//!
//! `orbit --root <dir> workspace init` registers a checkout whose Orbit root
//! lives outside the repository, so nothing under the checkout marks it as an
//! Orbit workspace. Only the workspace registry in that root knows the pair,
//! and these tests pin that both setup commands resolve the checkout through
//! it: the client config lands in the repository, carries the registered
//! `ws_*` binding, and is removed again — while a root that names no single
//! registered checkout refuses instead of writing somewhere else (ORB-12121).

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

/// One machine with an external Orbit root and a single checkout registered
/// against it.
struct ExternalRootFixture {
    _temp: TempDir,
    home: PathBuf,
    orbit_root: PathBuf,
    checkout: PathBuf,
    elsewhere: PathBuf,
}

impl ExternalRootFixture {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        // Deliberately nested, so a write to the root's parent is visible.
        let orbit_root = temp.path().join("orbit-data").join("root");
        let checkout = temp.path().join("checkout");
        let elsewhere = temp.path().join("elsewhere");
        for directory in [&home, &elsewhere] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        init_git_repo(&checkout);

        let fixture = Self {
            _temp: temp,
            home,
            orbit_root,
            checkout,
            elsewhere,
        };
        fixture
            .orbit(
                &fixture.checkout,
                &fixture.rooted(&[
                    "init",
                    "--non-interactive",
                    "--host-name",
                    "external-root-host",
                    "--task-prefix",
                    "EXR",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout,
                &fixture.rooted(&["workspace", "init", "--name", "wsname"]),
            )
            .success();
        fixture.assert_external_root_layout();
        fixture
    }

    /// Confirm the setup produced the layout these tests are about: the
    /// registry in `orbit_root` binds the checkout, and the checkout itself
    /// carries no `.orbit` for a filesystem walk-up to find.
    fn assert_external_root_layout(&self) {
        let assert = self
            .orbit(
                &self.checkout,
                &self.rooted(&["workspace", "show", "--format", "json"]),
            )
            .success();
        let shown: Value =
            serde_json::from_slice(&assert.get_output().stdout).expect("workspace show json");
        assert_eq!(shown["workspace"]["id"], "ws_wsname");
        assert_eq!(
            shown["checkout"]["repo_root"],
            canonical_str(&self.checkout)
        );
        assert_eq!(
            shown["checkout"]["orbit_dir"],
            canonical_str(&self.orbit_root)
        );
        assert!(!self.checkout.join(".orbit").exists());
    }

    /// Prefix `args` with the explicit data-root selector this layout requires.
    fn rooted(&self, args: &[&str]) -> Vec<String> {
        let mut rooted = vec![
            "--root".to_string(),
            self.orbit_root
                .to_str()
                .expect("utf8 orbit root")
                .to_string(),
        ];
        rooted.extend(argv(args));
        rooted
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        self.orbit_with_env(cwd, args, &[])
    }

    fn orbit_with_env(
        &self,
        cwd: &Path,
        args: &[String],
        env: &[(&str, &Path)],
    ) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.assert()
    }

    fn claude_config(&self) -> PathBuf {
        self.checkout.join(".mcp.json")
    }

    fn claude_settings(&self) -> PathBuf {
        self.checkout.join(".claude").join("settings.json")
    }

    /// Every directory that a mis-resolved run has historically written into.
    fn assert_no_client_config_outside_the_checkout(&self) {
        let root_parent = self.orbit_root.parent().expect("orbit root parent");
        for directory in [&self.orbit_root, root_parent, &self.elsewhere, &self.home] {
            for entry in [".mcp.json", ".claude.json", ".claude"] {
                let stray = directory.join(entry);
                assert!(
                    !stray.exists(),
                    "MCP client config written outside the checkout: {}",
                    stray.display()
                );
            }
        }
    }
}

#[test]
fn mcp_init_binds_an_external_root_checkout_and_remove_reverses_it() {
    let fixture = ExternalRootFixture::init();

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "init", "--claude"]),
        )
        .success();

    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        vec!["mcp", "serve", "--workspace", "ws_wsname"],
        "the generated server must carry the registered workspace binding"
    );
    assert!(fixture.claude_settings().is_file());
    fixture.assert_no_client_config_outside_the_checkout();

    fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["mcp", "remove", "--claude"]),
        )
        .success();

    assert!(!fixture.claude_config().exists());
    assert!(!fixture.checkout.join(".claude").exists());
}

#[test]
fn external_root_task_index_loss_is_reported_and_reindexed() {
    let fixture = ExternalRootFixture::init();
    let added = fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&[
                "task",
                "add",
                "--title",
                "External task",
                "--complexity",
                "low",
                "--json",
            ]),
        )
        .success();
    let task: Value = serde_json::from_slice(&added.get_output().stdout).expect("task json");
    let task_id = task["id"].as_str().expect("task id").to_string();
    let index = fixture.orbit_root.join("tasks/index.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{}", index.display(), suffix));
        if path.exists() {
            fs::remove_file(path).expect("remove task index");
        }
    }

    let listed = fixture
        .orbit(&fixture.checkout, &fixture.rooted(&["task", "list"]))
        .failure();
    let stderr = String::from_utf8_lossy(&listed.get_output().stderr);
    assert!(
        stderr.contains("error: store error: task index is missing 1 on-disk bundle(s); run `orbit task reindex` to recover them"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("invalid input"), "stderr: {stderr}");

    let listed_json = fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["task", "list", "--format", "json"]),
        )
        .failure();
    let stderr_json = String::from_utf8_lossy(&listed_json.get_output().stderr);
    let error_payload: Value = serde_json::from_str(stderr_json.trim()).expect("error json");
    assert_eq!(error_payload["code"], "store_error", "json: {stderr_json}");
    assert_eq!(
        error_payload["error"],
        "store error: task index is missing 1 on-disk bundle(s); run `orbit task reindex` to recover them",
        "json: {stderr_json}"
    );
    assert!(
        !stderr_json.contains("invalid_input"),
        "json: {stderr_json}"
    );

    fixture
        .orbit(&fixture.checkout, &fixture.rooted(&["task", "reindex"]))
        .success();
    let listed = fixture
        .orbit(
            &fixture.checkout,
            &fixture.rooted(&["task", "list", "--json"]),
        )
        .success();
    let tasks: Value = serde_json::from_slice(&listed.get_output().stdout).expect("task list json");
    assert_eq!(tasks[0]["id"], task_id);
}

#[test]
fn mcp_init_resolves_the_registered_checkout_from_an_unrelated_directory() {
    let fixture = ExternalRootFixture::init();

    fixture
        .orbit(
            &fixture.elsewhere,
            &fixture.rooted(&["mcp", "init", "--claude"]),
        )
        .success();

    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        vec!["mcp", "serve", "--workspace", "ws_wsname"]
    );
    fixture.assert_no_client_config_outside_the_checkout();

    fixture
        .orbit(
            &fixture.elsewhere,
            &fixture.rooted(&["mcp", "remove", "--claude"]),
        )
        .success();

    assert!(!fixture.claude_config().exists());
    assert!(!fixture.checkout.join(".claude").exists());
}

#[test]
fn orbit_root_env_selects_the_same_checkout_as_the_root_flag() {
    let fixture = ExternalRootFixture::init();
    let env = [("ORBIT_ROOT", fixture.orbit_root.as_path())];

    fixture
        .orbit_with_env(&fixture.checkout, &argv(&["mcp", "init", "--claude"]), &env)
        .success();

    assert_eq!(
        generated_server_args(&fixture.claude_config()),
        vec!["mcp", "serve", "--workspace", "ws_wsname"]
    );
    fixture.assert_no_client_config_outside_the_checkout();

    fixture
        .orbit_with_env(
            &fixture.checkout,
            &argv(&["mcp", "remove", "--claude"]),
            &env,
        )
        .success();

    assert!(!fixture.claude_config().exists());
}

#[test]
fn explicit_root_outranks_a_different_registered_cwd_checkout() {
    let fixture = RegisteredCheckoutPairFixture::init();
    let root = fixture
        .checkout_b
        .join(".orbit")
        .canonicalize()
        .expect("canonicalize B Orbit root");

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&[
                "--root",
                root.to_str().expect("utf8 B Orbit root"),
                "mcp",
                "init",
                "--claude",
            ]),
        )
        .success();

    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert_eq!(
        generated_server_args(&fixture.checkout_b.join(".mcp.json")),
        vec!["mcp", "serve", "--workspace", "ws_beta"]
    );

    let env = [("ORBIT_ROOT", root.as_path())];
    fixture
        .orbit_with_env(
            &fixture.checkout_a,
            &argv(&["mcp", "remove", "--claude"]),
            &env,
        )
        .success();

    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(!fixture.checkout_b.join(".mcp.json").exists());
}

#[test]
fn shared_root_mcp_setup_uses_cwd_checkout_for_init_and_remove() {
    let fixture = SharedRootCheckoutPairFixture::init();
    let root = fixture.orbit_root.to_str().expect("utf8 shared Orbit root");

    let outside = fixture
        .orbit(
            &fixture.elsewhere,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .failure();
    let stderr = String::from_utf8_lossy(&outside.get_output().stderr);
    assert!(
        stderr.contains("does not identify exactly one registered checkout"),
        "unexpected outside-checkout failure: {stderr}"
    );
    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(!fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .success();
    assert_eq!(
        generated_server_args(&fixture.checkout_a.join(".mcp.json")),
        vec!["mcp", "serve", "--workspace", "ws_alpha"]
    );
    assert!(!fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_b,
            &argv(&["--root", root, "mcp", "init", "--claude"]),
        )
        .success();
    assert_eq!(
        generated_server_args(&fixture.checkout_b.join(".mcp.json")),
        vec!["mcp", "serve", "--workspace", "ws_beta"]
    );

    fixture
        .orbit(
            &fixture.checkout_a,
            &argv(&["--root", root, "mcp", "remove", "--claude"]),
        )
        .success();
    assert!(!fixture.checkout_a.join(".mcp.json").exists());
    assert!(fixture.checkout_b.join(".mcp.json").exists());

    fixture
        .orbit(
            &fixture.checkout_b,
            &argv(&["--root", root, "mcp", "remove", "--claude"]),
        )
        .success();
    assert!(!fixture.checkout_b.join(".mcp.json").exists());
}

#[test]
fn a_root_without_a_registered_checkout_refuses_instead_of_writing() {
    let fixture = ExternalRootFixture::init();
    let unrelated_root = fixture
        .orbit_root
        .parent()
        .expect("orbit root parent")
        .join("unregistered");
    fs::create_dir_all(&unrelated_root).expect("create unregistered root");

    let assert = fixture
        .orbit(
            &fixture.elsewhere,
            &argv(&[
                "--root",
                unrelated_root.to_str().expect("utf8 root"),
                "mcp",
                "init",
                "--claude",
            ]),
        )
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("does not identify exactly one registered checkout"),
        "unexpected failure message: {stderr}"
    );
    assert!(!unrelated_root.join(".mcp.json").exists());
    fixture.assert_no_client_config_outside_the_checkout();
    assert!(!fixture.claude_config().exists());
}

#[test]
fn an_unregistered_root_reports_the_root_and_registered_candidates() {
    let fixture = RegisteredCheckoutPairFixture::init();
    let root = fixture.checkout_a.join("not-registered");
    let assert = fixture.orbit(
        &fixture.checkout_a,
        &argv(&[
            "--root",
            root.to_str().expect("utf8 unregistered root"),
            "mcp",
            "init",
            "--claude",
        ]),
    );
    let stderr = String::from_utf8_lossy(&assert.failure().get_output().stderr).to_string();

    assert!(
        stderr.contains(root.to_str().expect("utf8 unregistered root")),
        "failure must name the explicit root: {stderr}"
    );
    assert!(
        stderr.contains(fixture.checkout_a.to_str().expect("utf8 checkout A"))
            && stderr.contains(fixture.checkout_b.to_str().expect("utf8 checkout B")),
        "failure must name candidate checkouts: {stderr}"
    );
}

struct SharedRootCheckoutPairFixture {
    _temp: TempDir,
    home: PathBuf,
    orbit_root: PathBuf,
    checkout_a: PathBuf,
    checkout_b: PathBuf,
    elsewhere: PathBuf,
}

impl SharedRootCheckoutPairFixture {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let orbit_root = temp.path().join("shared-root");
        let checkout_a = temp.path().join("checkout-a");
        let checkout_b = temp.path().join("checkout-b");
        let elsewhere = temp.path().join("elsewhere");
        for directory in [&home, &elsewhere] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        init_git_repo(&checkout_a);
        init_git_repo(&checkout_b);

        let fixture = Self {
            _temp: temp,
            home,
            orbit_root,
            checkout_a,
            checkout_b,
            elsewhere,
        };
        let root = fixture.orbit_root.to_str().expect("utf8 shared Orbit root");
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&[
                    "--root",
                    root,
                    "init",
                    "--non-interactive",
                    "--host-name",
                    "shared-root-host",
                    "--task-prefix",
                    "SHR",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&["--root", root, "workspace", "init", "--name", "alpha"]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_b,
                &argv(&["--root", root, "workspace", "init", "--name", "beta"]),
            )
            .success();
        fixture.assert_shared_root_layout();
        fixture
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .args(args)
            .assert()
    }

    fn assert_shared_root_layout(&self) {
        for (checkout, workspace_id) in [
            (&self.checkout_a, "ws_alpha"),
            (&self.checkout_b, "ws_beta"),
        ] {
            let assert = self
                .orbit(
                    checkout,
                    &argv(&[
                        "--root",
                        self.orbit_root.to_str().expect("utf8 shared Orbit root"),
                        "workspace",
                        "show",
                        "--format",
                        "json",
                    ]),
                )
                .success();
            let shown: Value =
                serde_json::from_slice(&assert.get_output().stdout).expect("workspace show json");
            assert_eq!(shown["workspace"]["id"], workspace_id);
            assert_eq!(shown["checkout"]["repo_root"], canonical_str(checkout));
            assert_eq!(
                shown["checkout"]["orbit_dir"],
                canonical_str(&self.orbit_root)
            );
            assert!(!checkout.join(".orbit").exists());
        }
    }
}

struct RegisteredCheckoutPairFixture {
    _temp: TempDir,
    home: PathBuf,
    checkout_a: PathBuf,
    checkout_b: PathBuf,
}

impl RegisteredCheckoutPairFixture {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let checkout_a = temp.path().join("checkout-a");
        let checkout_b = temp.path().join("checkout-b");
        fs::create_dir_all(&home).expect("fixture home");
        init_git_repo(&checkout_a);
        init_git_repo(&checkout_b);

        let fixture = Self {
            _temp: temp,
            home,
            checkout_a,
            checkout_b,
        };
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&[
                    "init",
                    "--non-interactive",
                    "--host-name",
                    "local-root-host",
                    "--task-prefix",
                    "LCL",
                ]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_a,
                &argv(&["workspace", "init", "--name", "alpha"]),
            )
            .success();
        fixture
            .orbit(
                &fixture.checkout_b,
                &argv(&["workspace", "init", "--name", "beta"]),
            )
            .success();
        fixture
    }

    fn orbit(&self, cwd: &Path, args: &[String]) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .args(args)
            .assert()
    }

    fn orbit_with_env(
        &self,
        cwd: &Path,
        args: &[String],
        env: &[(&str, &Path)],
    ) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command.assert()
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn canonical_str(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonicalize fixture path")
        .to_str()
        .expect("utf8 fixture path")
        .to_string()
}

fn generated_server_args(config_path: &Path) -> Vec<String> {
    let config: Value = serde_json::from_str(
        &fs::read_to_string(config_path).expect("read generated client config"),
    )
    .expect("parse generated client config");
    config["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("generated args array")
        .iter()
        .map(|value| value.as_str().expect("arg is a string").to_string())
        .collect()
}

fn init_git_repo(repo: &Path) {
    fs::create_dir_all(repo).expect("create repo");
    run_git(repo, &["init", "--initial-branch", "main"]);
    run_git(repo, &["config", "user.name", "Orbit Test"]);
    run_git(repo, &["config", "user.email", "orbit-test@example.com"]);
    run_git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join("README.md"), "# repo\n").expect("write readme");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-m", "initial"]);
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
