//! `orbit doctor providers` reports whether each executor's effective
//! arguments yield tool-call traces, apart from binary availability. The
//! observed regression: an installed grok executor still passing
//! `--output-format json` (one result document, no tool frames) delivered
//! successful changes while every run had an empty trace, and the diagnostic
//! said only "installed".

use std::fs;
use std::path::Path;

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use serde_json::Value;

use crate::git_repo;

struct Qa {
    _temp: tempfile::TempDir,
    home: std::path::PathBuf,
    repo: std::path::PathBuf,
}

impl Qa {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let qa = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            _temp: temp,
        };
        fs::create_dir_all(&qa.home).unwrap();
        git_repo::init(&qa.repo);
        qa.command(&["workspace", "init", "--name", "telemetry-qa"])
            .assert()
            .success();
        qa
    }

    fn command(&self, args: &[&str]) -> assert_cmd::Command {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .arg("--root")
            .arg(self.repo.join(".orbit"))
            .args(args);
        command
    }

    fn grok_yaml(&self) -> std::path::PathBuf {
        self.repo.join(".orbit/resources/executors/grok.yaml")
    }

    fn set_args(&self, args: &[&str]) {
        let path = self.grok_yaml();
        let mut executor: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        executor["spec"]["args"] = serde_yaml::to_value(args).unwrap();
        fs::write(&path, serde_yaml::to_string(&executor).unwrap()).unwrap();
    }

    fn providers(&self) -> (Value, String) {
        let json = self
            .command(&["doctor", "providers", "--json"])
            .output()
            .unwrap();
        assert!(json.status.success(), "{json:?}");
        let rows: Value = serde_json::from_slice(&json.stdout).unwrap();
        let text = self.command(&["doctor", "providers"]).output().unwrap();
        assert!(text.status.success(), "{text:?}");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&text.stdout),
            String::from_utf8_lossy(&text.stderr)
        );
        (rows, text)
    }
}

fn row<'a>(rows: &'a Value, name: &str) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == name)
        .unwrap_or_else(|| panic!("no {name} executor: {rows:#}"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn grok_json_output_is_reported_as_no_tool_telemetry_without_failing_the_provider() {
    let qa = Qa::new();

    // The shipped definition selects streaming-json.
    let (rows, _) = qa.providers();
    let grok = row(&rows, "grok");
    assert_eq!(grok["tool_telemetry"]["status"], "supported", "{grok}");
    assert_eq!(grok["tool_telemetry"]["output_format"], "streaming-json");

    // The audited installed definition: single-document JSON.
    qa.set_args(&[
        "--permission-mode",
        "bypassPermissions",
        "--output-format",
        "json",
        "--no-plan",
        "--prompt-file",
        "/dev/stdin",
    ]);
    let before = read(&qa.grok_yaml());
    let (rows, text) = qa.providers();
    let grok = row(&rows, "grok");
    let telemetry = &grok["tool_telemetry"];
    assert_eq!(telemetry["status"], "unavailable", "{grok}");
    assert_eq!(telemetry["output_format"], "json", "{grok}");
    assert!(
        telemetry["cause"]
            .as_str()
            .unwrap()
            .contains("answer and usage are read from that document"),
        "{grok}"
    );
    assert!(
        telemetry["remediation"]
            .as_str()
            .unwrap()
            .contains("--output-format streaming-json"),
        "{grok}"
    );
    // Binary and sandbox reporting is independent of the transport verdict.
    assert_eq!(grok["availability"], row(&rows, "claude")["availability"]);
    assert!(
        text.lines()
            .any(|line| line.starts_with("grok\t") && line.contains("unavailable (json)")),
        "{text}"
    );
    assert!(
        text.contains("grok tool telemetry unavailable (json)") && text.contains("streaming-json"),
        "{text}"
    );
    assert_eq!(
        read(&qa.grok_yaml()),
        before,
        "the diagnostic must not rewrite the executor"
    );

    // A custom or absent format is qualified, never claimed to supply traces.
    for args in [&["--output-format", "text"][..], &["--no-plan"][..]] {
        qa.set_args(args);
        let (rows, _) = qa.providers();
        assert_eq!(
            row(&rows, "grok")["tool_telemetry"]["status"],
            "unknown",
            "{args:?}"
        );
    }
    // Other providers are not asserted to record tool calls either.
    let (rows, _) = qa.providers();
    assert_eq!(row(&rows, "claude")["tool_telemetry"]["status"], "unknown");
    assert_eq!(
        row(&rows, "local-shell")["tool_telemetry"]["status"],
        "not_applicable"
    );
}
