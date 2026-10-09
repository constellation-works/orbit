//! `orbit --root <dir> config set` with no distinct workspace layer.
//!
//! `--root` pins one config root for both layers, so a bare `config set` has
//! no workspace file to edit. It must refuse with a message that names
//! `--global` and the file that flag edits, and leave every config file
//! untouched; `--global` against the same root must then apply the edit.

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    root: PathBuf,
}

impl Fixture {
    fn initialized_with_workspace() -> Self {
        let temp = tempdir().expect("tempdir");
        let fixture = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            root: temp.path().join("scratch-root"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).expect("home");
        fs::create_dir_all(&fixture.repo).expect("repo");
        init_git_repo(&fixture.repo);
        let root = fixture.root.to_string_lossy().into_owned();
        fixture
            .orbit(&[
                "--root",
                &root,
                "init",
                "--non-interactive",
                "--machine-name",
                "config-root-host",
                "--task-prefix",
                "CR",
            ])
            .success();
        fixture
            .orbit(&["--root", &root, "workspace", "init"])
            .success();
        fixture
    }

    fn global_config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    fn orbit(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .args(args);
        command.assert()
    }
}

#[test]
fn config_set_under_root_refuses_without_workspace_layer_and_names_global() {
    let fixture = Fixture::initialized_with_workspace();
    let root = fixture.root.to_string_lossy().into_owned();
    let before = fs::read_to_string(fixture.global_config()).expect("global config");

    let refused = fixture
        .orbit(&[
            "--root",
            &root,
            "config",
            "set",
            "workflow.base_branch",
            "agent-main",
        ])
        .failure();
    let stderr = String::from_utf8_lossy(&refused.get_output().stderr).into_owned();
    assert!(
        stderr.contains("--global"),
        "refusal must name the --global remedy: {stderr}"
    );
    assert!(
        stderr.contains(&*fixture.global_config().to_string_lossy()),
        "refusal must name the file --global edits: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(fixture.global_config()).expect("global config"),
        before,
        "a refused bare set must not write the override root's config"
    );

    fixture
        .orbit(&[
            "--root",
            &root,
            "config",
            "set",
            "--global",
            "workflow.base_branch",
            "agent-main",
        ])
        .success();
    let written = fs::read_to_string(fixture.global_config()).expect("global config");
    assert!(
        written.contains("agent-main"),
        "--global must apply the edit the refusal pointed at: {written}"
    );
}

fn init_git_repo(repo: &Path) {
    let status = StdCommand::new("git")
        .args(["init", "--quiet", "-b", "agent-main"])
        .current_dir(repo)
        .status()
        .expect("git init");
    assert!(status.success());
    let status = StdCommand::new("git")
        .args(["commit", "--quiet", "--allow-empty", "-m", "init"])
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .status()
        .expect("git commit");
    assert!(status.success());
}
