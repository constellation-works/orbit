//! Disposable CLI home, registry, workspace, and bounded argv evidence shared by family tests.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

pub(crate) struct Fixture {
    pub(crate) _temp: TempDir,
    pub(crate) home: PathBuf,
    pub(crate) repo: PathBuf,
    pub(crate) root: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let temp = tempdir().unwrap();
        let fixture = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            root: temp.path().join("state"),
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).unwrap();
        fs::create_dir_all(&fixture.repo).unwrap();
        let output = crate::git_repo::command()
            .args(["init", "--quiet"])
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        fixture
            .command(&[
                "init",
                "--non-interactive",
                "--machine-name",
                "audit-qa",
                "--task-prefix",
                "AQ",
            ])
            .assert()
            .success();
        fixture
            .command(&["workspace", "init", "--name", "audit-qa"])
            .assert()
            .success();
        fixture.json(&[
            "task",
            "add",
            "--title",
            "Audit fixture",
            "--complexity",
            "low",
            "--acceptance-criteria",
            "Persist audit evidence",
            "--json",
        ]);
        fixture
    }

    pub(crate) fn command(&self, args: &[&str]) -> assert_cmd::Command {
        self.command_at_root(&self.root, args)
    }

    pub(crate) fn command_at_root(
        &self,
        root: &std::path::Path,
        args: &[&str],
    ) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"));
        if !root.as_os_str().is_empty() {
            command.arg("--root").arg(root);
        }
        command.args(args);
        command
    }

    pub(crate) fn json(&self, args: &[&str]) -> Value {
        let output = self
            .command(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        if std::env::var_os("ORBIT_QA_TRACE_CLI").is_some() {
            writeln!(
                std::io::stderr(),
                "QA_CLI {}",
                serde_json::json!({
                    "test": std::thread::current().name(), "argv": args, "exit_code": 0,
                })
            )
            .expect("write optional CLI evidence");
        }
        serde_json::from_slice(&output).unwrap_or_else(|error| {
            panic!(
                "JSON for {args:?}: {error}; {}",
                String::from_utf8_lossy(&output)
            )
        })
    }
}
