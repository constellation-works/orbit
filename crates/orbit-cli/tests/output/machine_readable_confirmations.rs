#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Confirmation-style commands (init, config, tool toggles, task update) must
//! honor `--format json` with one JSON document on stdout, and refuse inputs
//! that used to be persisted or reported as success without effect.
//!
//! Every command runs as a child process against a disposable `HOME` and
//! checkout, per `docs/DEVELOPMENT.md#safe-mutable-cli-fixtures`.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use assert_cmd::cargo::cargo_bin_cmd;
use orbit_common::test_env;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(&work).expect("create work");
        for args in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Orbit Test"],
            &["config", "user.email", "orbit-test@example.com"],
            &["config", "commit.gpgsign", "false"],
            &["commit", "--quiet", "--allow-empty", "-m", "initial"],
        ] {
            let output = crate::git_repo::command()
                .arg("-C")
                .arg(&work)
                .args(args)
                .output()
                .expect("run git");
            assert!(output.status.success(), "git {args:?} failed");
        }
        Self {
            _temp: temp,
            home,
            work,
        }
    }

    fn orbit(&self) -> Command {
        fixture_orbit(&self.work, &self.home)
    }

    #[cfg(unix)]
    fn orbit_with_fake_clock_manager(&self) -> Command {
        use std::os::unix::fs::PermissionsExt;

        let bin = self._temp.path().join("clock-manager-bin");
        fs::create_dir_all(&bin).expect("create fake clock manager directory");
        let (program, script) = if cfg!(target_os = "macos") {
            (
                "launchctl",
                r#"
case "$*" in
  "list com.orbit.sweep")
    if [ -f "$HOME/.fake-clock-enabled" ]; then exit 0; fi
    printf 'Could not find service "com.orbit.sweep"\n' >&2; exit 1 ;;
  unload*)
    rm -f "$HOME/.fake-clock-enabled"; exit 0 ;;
  load*)
    fail_activation
    touch "$HOME/.fake-clock-enabled"; exit 0 ;;
  *) exit 0 ;;
esac
"#,
            )
        } else {
            (
                "systemctl",
                r#"
case "$*" in
  "--user is-enabled orbit-sweep.timer")
    if [ -f "$HOME/.fake-clock-enabled" ]; then printf 'enabled\n'; exit 0; fi
    if [ -f "$HOME/.config/systemd/user/orbit-sweep.timer" ]; then
      printf 'disabled\n' >&2
    else
      printf 'Failed to get unit file state: No such file or directory\n' >&2
    fi; exit 1 ;;
  *"show orbit-sweep.service"*)
    printf 'ActiveState=inactive\nExecMainStartTimestampMonotonic=0\nResult=success\nTimeoutStartUSec=10min\nKillMode=mixed\n'; exit 0 ;;
  *"show orbit-sweep.timer"*)
    if [ -f "$HOME/.fake-clock-enabled" ]; then
      printf 'LoadState=loaded\nActiveState=active\nNextElapseUSecMonotonic=999999999999999999\n'
    else
      printf 'LoadState=loaded\nActiveState=inactive\nNextElapseUSecMonotonic=\n'
    fi; exit 0 ;;
  "--user disable --now orbit-sweep.timer")
    rm -f "$HOME/.fake-clock-enabled"; exit 0 ;;
  "--user enable orbit-sweep.timer")
    fail_activation
    touch "$HOME/.fake-clock-enabled"; exit 0 ;;
  "--user restart orbit-sweep.timer")
    touch "$HOME/.fake-clock-enabled"; exit 0 ;;
  *) exit 0 ;;
esac
"#,
            )
        };
        // Fail exactly one activation, then let the public command roll back
        // through the same manager. All state belongs to the disposable HOME.
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/.fake-clock-commands"
fail_activation() {{
  if [ -f "$HOME/.fake-clock-fail-activation" ]; then
    rm "$HOME/.fake-clock-fail-activation"
    printf 'injected activation failure\n' >&2
    exit 1
  fi
}}
{script}"#
        );
        let path = bin.join(program);
        fs::write(&path, script).expect("write fake clock manager");
        let mut permissions = fs::metadata(&path)
            .expect("inspect fake clock manager")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("make fake clock manager executable");

        let mut paths = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect::<Vec<_>>();
        paths.insert(0, bin);
        let mut command = self.orbit();
        command.env(
            "PATH",
            std::env::join_paths(paths).expect("join fake manager PATH"),
        );
        command
    }

    #[cfg(unix)]
    fn json_with_fake_clock_manager(&self, args: &[&str]) -> Value {
        let output = self
            .orbit_with_fake_clock_manager()
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`orbit {}` must print one JSON document: {error}\nstdout:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self
            .orbit()
            .args(args)
            .assert()
            .success()
            .get_output()
            .clone();
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "`orbit {}` must print one JSON document: {error}\nstdout:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn init_machine_and_workspace(&self) {
        self.orbit()
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
                "qa-host",
                "--task-prefix",
                "QA",
            ])
            .assert()
            .success();
        self.orbit()
            .args(["workspace", "init", "--name", "qa-workspace"])
            .assert()
            .success();
    }
}

fn fixture_orbit(work: &Path, home: &Path) -> Command {
    let mut command = cargo_bin_cmd!("orbit");
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("ORBIT_SKIP_HOST_PREREQUISITES", "1")
        .env_remove("ORBIT_FORMAT")
        .env_remove("NO_COLOR")
        .timeout(std::time::Duration::from_secs(120));
    command
}

#[test]
fn init_reports_the_machine_identity_as_json_and_flags_ignored_overrides() {
    let fixture = Fixture::new();

    let created = fixture.json(&[
        "init",
        "--non-interactive",
        "--machine-name",
        "qa-host",
        "--task-prefix",
        "QA",
        "--format",
        "json",
    ]);
    assert_eq!(created["machine"]["outcome"], "created");
    assert_eq!(created["machine"]["name"], "qa-host");
    assert_eq!(created["machine"]["task_prefix"], "QA");
    assert!(
        created["machine"]["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("hm_")),
        "machine id missing: {created}"
    );
    assert_eq!(created["warnings"], serde_json::json!([]));

    // The prefix is immutable: a differing flag on a re-run is not applied, and
    // saying nothing would let the operator believe it was.
    let repeated = fixture
        .orbit()
        .args([
            "init",
            "--non-interactive",
            "--task-prefix",
            "ZZ",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("--task-prefix"))
        .get_output()
        .clone();
    let repeated: Value = serde_json::from_slice(&repeated.stdout).expect("re-init JSON");
    assert_eq!(repeated["machine"]["outcome"], "unchanged");
    assert_eq!(repeated["machine"]["task_prefix"], "QA");
}

#[test]
fn config_path_and_set_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let path = fixture.json(&["config", "path", "--global", "--format", "json"]);
    assert!(
        path["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("config.toml")),
        "config path document: {path}"
    );

    let set = fixture.json(&[
        "config",
        "set",
        "--global",
        "automation.stall_window_minutes",
        "45",
        "--format",
        "json",
    ]);
    assert_eq!(set["key"], "automation.stall_window_minutes");
    assert_eq!(set["scope"], "global");
    let value = fixture.json(&[
        "config",
        "get",
        "automation.stall_window_minutes",
        "--format",
        "json",
    ]);
    assert_eq!(value["value"], 45);
}

#[cfg(unix)]
impl Fixture {
    fn clock_unit_path(&self) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home.join("Library/LaunchAgents/com.orbit.sweep.plist")
        } else {
            self.home.join(".config/systemd/user/orbit-sweep.timer")
        }
    }

    fn assert_native_cadence(&self, cadence: u64) {
        let unit = fs::read_to_string(self.clock_unit_path()).expect("read installed clock unit");
        let expected = if cfg!(target_os = "macos") {
            format!("<integer>{cadence}</integer>")
        } else {
            format!("OnUnitActiveSec={cadence}s")
        };
        assert!(unit.lines().any(|line| line.trim() == expected), "{unit}");
    }

    fn clock_manager_commands(&self) -> Vec<String> {
        fs::read_to_string(self.home.join(".fake-clock-commands"))
            .expect("read fake manager commands")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn set_clock_cadence(&self, cadence: &str) -> Value {
        self.json_with_fake_clock_manager(&["clock", "set", "--cadence-seconds", cadence, "--json"])
    }
}

#[cfg(unix)]
#[test]
fn clock_cadence_change_keeps_a_paused_clock_paused() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    // Install through the CLI, then pause as an operator would.
    assert_eq!(fixture.set_clock_cadence("120")["activated"], true);
    fixture.json_with_fake_clock_manager(&["clock", "pause", "--json"]);
    let before = fixture.clock_manager_commands().len();

    let set = fixture.set_clock_cadence("300");
    assert_eq!(set["configured_cadence_seconds"], 300);
    assert_eq!(set["paused"], true);
    assert_eq!(set["activated"], false);
    let probe = if cfg!(target_os = "macos") {
        "list com.orbit.sweep"
    } else {
        "--user is-enabled orbit-sweep.timer"
    };
    assert_eq!(
        &fixture.clock_manager_commands()[before..],
        &[probe],
        "a paused cadence change must only observe the manager, never reload or activate it"
    );

    let status = fixture.json_with_fake_clock_manager(&["clock", "status", "--json"]);
    assert_eq!(status["enabled"], false, "cadence change resumed the clock");
    assert_eq!(status["configured_cadence_seconds"], 300);
    fixture.assert_native_cadence(300);
}

#[cfg(unix)]
#[test]
fn clock_cadence_change_activates_enabled_and_never_installed_clocks() {
    for initially_enabled in [true, false] {
        let fixture = Fixture::new();
        fixture.init_machine_and_workspace();
        assert!(
            !fixture.clock_unit_path().exists(),
            "fixture must start uninstalled"
        );
        let before = if initially_enabled {
            assert_eq!(fixture.set_clock_cadence("120")["activated"], true);
            fixture.clock_manager_commands().len()
        } else {
            0
        };

        let set = fixture.set_clock_cadence("300");
        assert_eq!(set["activated"], true, "{set}");
        assert_eq!(set["paused"], false, "{set}");
        assert_eq!(set["configured_cadence_seconds"], 300);
        let commands = fixture.clock_manager_commands();
        let activation = &commands[before..];
        if cfg!(target_os = "macos") {
            assert!(
                activation
                    .iter()
                    .any(|command| command.starts_with("unload ")),
                "{activation:?}"
            );
            assert!(
                activation
                    .iter()
                    .any(|command| command.starts_with("load ")),
                "{activation:?}"
            );
        } else {
            for command in [
                "--user daemon-reload",
                "--user enable orbit-sweep.timer",
                "--user restart orbit-sweep.timer",
            ] {
                assert!(
                    activation.iter().any(|actual| actual == command),
                    "{activation:?}"
                );
            }
        }
        let status = fixture.json_with_fake_clock_manager(&["clock", "status", "--json"]);
        assert_eq!(status["enabled"], true, "{status}");
        assert_eq!(status["configured_cadence_seconds"], 300);
        fixture.assert_native_cadence(300);
    }
}

#[cfg(unix)]
#[test]
fn clock_cadence_activation_failure_restores_the_previous_cadence() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    // A non-default previous cadence proves restoration, rather than reset.
    assert_eq!(fixture.set_clock_cadence("120")["activated"], true);
    fs::write(fixture.home.join(".fake-clock-fail-activation"), "")
        .expect("inject activation failure");
    let before = fixture.clock_manager_commands().len();

    fixture
        .orbit_with_fake_clock_manager()
        .args(["clock", "set", "--cadence-seconds", "300", "--json"])
        .assert()
        .failure();
    assert!(
        !fixture.home.join(".fake-clock-fail-activation").exists(),
        "the command must reach the injected manager activation failure"
    );
    let commands = fixture.clock_manager_commands();
    let activation_attempts = commands[before..]
        .iter()
        .filter(|command| {
            if cfg!(target_os = "macos") {
                command.starts_with("load ")
            } else {
                command.as_str() == "--user enable orbit-sweep.timer"
            }
        })
        .count();
    assert_eq!(
        activation_attempts, 2,
        "failed change and rollback: {commands:?}"
    );

    let status = fixture.json_with_fake_clock_manager(&["clock", "status", "--json"]);
    assert_eq!(status["configured_cadence_seconds"], 120, "{status}");
    assert_eq!(
        status["enabled"], true,
        "rollback must reactivate the previous unit: {status}"
    );
    fixture.assert_native_cadence(120);
}

#[cfg(unix)]
#[test]
fn clock_control_confirmations_are_json_and_keep_their_human_text() {
    let platform = if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd"
    };
    let cases: [(&[&str], &str, String); 3] = [
        (
            &["clock", "pause"],
            "pause",
            format!(
                "host scheduler clock paused ({platform}); manual `orbit clock tick` remains available\n"
            ),
        ),
        (
            &["clock", "enable"],
            "enable",
            format!("host scheduler clock enabled: runs every 60 seconds ({platform})\n"),
        ),
        (
            &["clock", "set", "--cadence-seconds", "1200"],
            "set_cadence",
            "host scheduler clock cadence set to 1200 seconds and reloaded\n".to_string(),
        ),
    ];

    for (command_args, action, expected_text) in cases {
        let fixture = Fixture::new();
        fixture.init_machine_and_workspace();

        let mut json_args = command_args.to_vec();
        json_args.push("--json");
        let json = fixture.json_with_fake_clock_manager(&json_args);
        assert_eq!(json["action"], action);
        match action {
            "pause" => {
                assert_eq!(json["enabled"], false);
                assert_eq!(json["manual_tick_available"], true);
                assert_eq!(json["platform"], platform);
            }
            "enable" => {
                assert_eq!(json["enabled"], true);
                assert_eq!(json["configured_cadence_seconds"], 60);
                assert_eq!(json["platform"], platform);
            }
            "set_cadence" => {
                assert_eq!(json["configured_cadence_seconds"], 1200);
                assert_eq!(json["activated"], true);
            }
            _ => unreachable!("each case names a clock confirmation action"),
        }

        let output = fixture
            .orbit_with_fake_clock_manager()
            .args(command_args)
            .assert()
            .success()
            .get_output()
            .clone();
        assert_eq!(
            String::from_utf8(output.stdout).expect("clock confirmation is UTF-8"),
            expected_text
        );
    }
}

#[test]
fn audit_json_export_renders_a_json_confirmation_and_preserves_csv_text() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let json_path = fixture._temp.path().join("audit.json");
    let json_path_text = json_path.to_string_lossy().into_owned();
    let exported = fixture.json(&[
        "audit",
        "export",
        "--format",
        "json",
        "--output",
        &json_path_text,
    ]);
    assert_eq!(exported["action"], "export");
    assert_eq!(exported["format"], "json");
    assert_eq!(exported["path"], json_path_text);
    let event_count = exported["events_exported"]
        .as_u64()
        .expect("export count is an integer");
    let rows: Vec<Value> = serde_json::from_slice(&fs::read(&json_path).expect("read JSON export"))
        .expect("export file is a JSON array");
    assert_eq!(rows.len() as u64, event_count);

    let csv_path = fixture._temp.path().join("audit.csv");
    let csv_path_text = csv_path.to_string_lossy().into_owned();
    let output = fixture
        .orbit()
        .args([
            "audit",
            "export",
            "--format",
            "csv",
            "--output",
            &csv_path_text,
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(output.stdout).expect("CSV confirmation is UTF-8");
    let details = text
        .strip_prefix("Exported ")
        .and_then(|text| text.strip_suffix('\n'))
        .expect("CSV export keeps its human confirmation");
    let (count, destination) = details
        .split_once(" events to ")
        .expect("human confirmation includes event count and destination");
    let count: u64 = count.parse().expect("human confirmation count");
    assert_eq!(destination, csv_path_text);
    assert_eq!(
        text,
        format!("Exported {count} events to {csv_path_text}\n")
    );
    assert!(csv_path.is_file(), "CSV export file is written");

    let default_path = fixture._temp.path().join("audit-default.json");
    let default_path_text = default_path.to_string_lossy().into_owned();
    let default_output = fixture
        .orbit()
        .args(["audit", "export", "--output", &default_path_text])
        .assert()
        .success()
        .get_output()
        .clone();
    let default_text =
        String::from_utf8(default_output.stdout).expect("default confirmation is UTF-8");
    assert!(
        default_text.starts_with("Exported ")
            && default_text.ends_with(&format!(" to {default_path_text}\n")),
        "the default file format keeps the human confirmation: {default_text:?}"
    );
}

/// `config set` validates before it writes: an invalid value, an unknown key
/// and a bad crew field each fail and leave both the workspace and the global
/// config byte-identical. A valid edit in the same fixture still lands.
#[test]
fn config_set_rejections_leave_config_files_byte_identical() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    let workspace_config = fixture.work.join(".orbit/config.toml");
    fs::write(
        &workspace_config,
        "# operator comment\n[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"gpt-test\"\n\n[execution.codex]\nsandbox = \"workspace-write\"\n",
    )
    .expect("write workspace config");
    let global_config = fixture.home.join(".orbit/config.toml");
    let snapshot = || {
        (
            fs::read(&workspace_config).expect("read workspace config"),
            fs::read(&global_config).expect("read global config"),
        )
    };
    let before = snapshot();

    for (label, args) in [
        (
            "invalid sandbox mode",
            &["execution.codex.sandbox", "not-a-real-mode"][..],
        ),
        ("unknown key", &["workflow.not_a_real_key", "value"]),
        ("misspelled crew field", &["crews.sol.effrot", "high"]),
        ("invalid crew effort", &["crews.sol.effort", "medium-low"]),
        ("non-bool crew flag", &["crews.sol.enabled", "maybe"]),
        (
            "invalid global value",
            &["--global", "automation.stall_window_minutes", "soon"],
        ),
    ] {
        fixture
            .orbit()
            .args(["config", "set"])
            .args(args)
            .assert()
            .failure();
        assert!(
            snapshot() == before,
            "{label}: a rejected `config set` must not write either config file"
        );
    }

    fixture
        .orbit()
        .args(["config", "set", "crews.sol.effort", "high"])
        .assert()
        .success();
    assert_ne!(
        fs::read(&workspace_config).expect("read workspace config"),
        before.0,
        "a valid edit still writes the workspace config"
    );
}

#[test]
fn workspace_config_set_validates_crews_against_the_effective_config() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let global_config = fixture.home.join(".orbit/config.toml");
    let mut global = fs::read_to_string(&global_config).expect("read global config");
    global.push_str(
        "\n[crews.global-only]\nenabled = true\nprovider = \"codex\"\nmodel = \"gpt-test\"\n",
    );
    fs::write(&global_config, global).expect("add global crew");

    let workspace_config = fixture.work.join(".orbit/config.toml");
    let workspace = "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\", \"global-only\"]\n\n[crews.sol]\nenabled = true\nprovider = \"codex\"\nmodel = \"gpt-test\"\n";
    fs::write(&workspace_config, workspace).expect("write workspace config");

    fixture
        .orbit()
        .args([
            "config",
            "set",
            "--global",
            "workflow.base_branch",
            "global-branch",
        ])
        .assert()
        .success();
    let shown = fixture.json(&["config", "show", "--scope", "workspace", "--json"]);
    assert_eq!(shown["source"]["scope"], "workspace");
    let pool = shown["settings"]["workflow.low_complexity_crews"]
        .as_array()
        .unwrap();
    assert_eq!(pool.len(), 2);
    assert!(
        pool.contains(&serde_json::json!("global-only")),
        "a workspace file view admits a global pool crew"
    );
    assert!(pool.contains(&serde_json::json!("sol")));
    assert_ne!(
        shown["settings"]["workflow.base_branch"], "global-branch",
        "the scoped view must not import other global settings"
    );
    fixture
        .orbit()
        .args(["config", "show", "--scope", "workspace"])
        .assert()
        .success();

    let partial_override =
        format!("{workspace}\n[crews.global-only]\nmodel = \"workspace-model\"\n");
    fs::write(&workspace_config, partial_override).expect("add partial crew override");
    fixture.json(&["config", "show", "--scope", "workspace", "--json"]);

    fixture
        .orbit()
        .args(["config", "set", "workflow.base_branch", "qa"])
        .assert()
        .success();
    let valid_workspace: toml::Value =
        toml::from_str(&fs::read_to_string(&workspace_config).expect("read workspace config"))
            .expect("parse workspace config");
    assert_eq!(
        valid_workspace["workflow"]["base_branch"].as_str(),
        Some("qa"),
        "workspace config set must persist an unrelated edit when its pool crew is global"
    );

    let invalid_workspace = "[workflow]\ndefault_crew = \"sol\"\nlow_complexity_crews = [\"sol\", \"global-only\", \"missing\"]\nbase_branch = \"qa\"\n\n[crews.sol]\nenabled = true\nprovider = \"codex\"\nmodel = \"gpt-test\"\n";
    fs::write(&workspace_config, invalid_workspace).expect("add undefined pool crew");
    let dangling = fixture
        .orbit()
        .args(["config", "show", "--scope", "workspace"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let reason = String::from_utf8_lossy(&dangling.stderr);
    assert!(reason.contains("workflow.low_complexity_crews"), "{reason}");
    assert!(reason.contains("missing"), "{reason}");
    let rejected = fixture
        .orbit()
        .args(["config", "set", "workflow.base_branch", "rejected"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("workflow.low_complexity_crews: crew 'missing' is not defined in [crews.*]"),
        "undefined crew must retain the effective-config error: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        fs::read_to_string(&workspace_config).expect("read rejected workspace config"),
        invalid_workspace,
        "a rejected workspace edit must not change the config file"
    );
}

#[test]
fn tool_toggles_and_mcp_registration_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let disabled = fixture.json(&["tool", "disable", "orbit.task.list", "--format", "json"]);
    assert_eq!(disabled["tool"], "orbit.task.list");
    assert_eq!(disabled["enabled"], false);
    let enabled = fixture.json(&["tool", "enable", "orbit.task.list", "--format", "json"]);
    assert_eq!(enabled["enabled"], true);

    let registered = fixture.json(&["mcp", "init", "--claude", "--format", "json"]);
    assert_eq!(registered["action"], "init");
    assert_eq!(registered["providers"], serde_json::json!(["claude"]));
    let removed = fixture.json(&["mcp", "remove", "--claude", "--format", "json"]);
    assert_eq!(removed["action"], "remove");
}

#[test]
fn task_update_refuses_empty_updates_and_unknown_pr_statuses() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    let id = String::from_utf8(
        fixture
            .orbit()
            .args(["task", "add", "--title", "qa", "--complexity", "low"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf8 task id")
    .trim()
    .to_string();

    fixture
        .orbit()
        .args(["task", "update", &id])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to update"));

    // A typo used to be stored verbatim as the task's review status.
    fixture
        .orbit()
        .args(["task", "update", &id, "--pr-status", "aproved"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown PR status 'aproved'"));
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert!(
        shown["pr_status"].is_null(),
        "a rejected value must not be persisted: {shown}"
    );

    for value in [
        "approve",
        "approved",
        "request-changes",
        "request_changes",
        "changes-requested",
        "changes_requested",
        " APPROVED ",
        " ApPrOvEd ",
        " ReQuEsT-ChAnGeS ",
        "",
        "   ",
    ] {
        if value.trim().is_empty() {
            fixture
                .orbit()
                .args(["task", "update", &id, "--pr-status", "approve"])
                .assert()
                .success();
        }
        fixture
            .orbit()
            .args(["task", "update", &id, "--pr-status", value])
            .assert()
            .success();
        let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
        let expected = (!value.trim().is_empty()).then_some(value.trim());
        assert_eq!(shown["pr_status"], serde_json::json!(expected), "{value:?}");
    }
}

#[test]
fn audit_prune_reports_a_bad_duration_before_asking_for_confirmation() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    fixture
        .orbit()
        .args(["audit", "prune", "--older-than", "garbage"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duration"));
}

#[test]
fn routine_init_tool_scaffold_and_workspace_teardown_print_json_documents() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();

    let routine = fixture.json(&["routine", "init", "--format", "json"]);
    assert_eq!(routine["machine"]["name"], "qa-host");
    assert_eq!(routine["clock"]["installed"], false);

    let script = fixture.work.join("tools").join("hello");
    let scaffold = fixture.json(&[
        "tool",
        "scaffold",
        script.to_str().expect("utf8 path"),
        "--name",
        "hello",
        "--format",
        "json",
    ]);
    assert_eq!(scaffold["tool"], "hello");
    assert!(script.is_file(), "scaffold must still write the executable");
    assert!(
        scaffold["executable"]
            .as_str()
            .is_some_and(|path| path.ends_with("hello")),
        "scaffold document: {scaffold}"
    );

    // Teardown needs the operator capability; the override is per-child here.
    fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args(["workspace", "teardown", "qa-workspace"])
        .assert()
        .failure();
    let output = fixture
        .orbit()
        .env("ORBIT_OPERATOR", "1")
        .args([
            "workspace",
            "teardown",
            "qa-workspace",
            "--confirm",
            "--format",
            "json",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let teardown: Value = serde_json::from_slice(&output.stdout).expect("teardown JSON");
    assert_eq!(teardown["workspace"], "qa-workspace");
    assert!(
        teardown["removed"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "teardown must list what it removed: {teardown}"
    );
    assert!(
        !fixture.work.join(".orbit").exists(),
        "teardown must still delete the data root"
    );
}

#[test]
fn root_pointing_at_a_file_is_refused_as_an_unusable_directory() {
    let fixture = Fixture::new();
    let file = fixture.work.join("not-a-dir");
    fs::write(&file, "x").expect("write file");

    fixture
        .orbit()
        .args(["--root", file.to_str().expect("utf8 path"), "task", "list"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a usable Orbit root"))
        .stderr(predicate::str::contains("admission").not());
}

#[test]
fn task_dependencies_warn_when_unreadable_like_parent_but_are_still_recorded() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    let known = String::from_utf8(
        fixture
            .orbit()
            .args([
                "task",
                "add",
                "--title",
                "dep",
                "--complexity",
                "low",
                "--tag",
                "no-diff-expected",
            ])
            .assert()
            .success()
            .stderr(predicate::str::contains("warning").not())
            .get_output()
            .stdout
            .clone(),
    )
    .expect("utf8 task id")
    .trim()
    .to_string();

    fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "with known dependency",
            "--complexity",
            "low",
            "--tag",
            "no-diff-expected",
            "--dependencies",
            &known,
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("warning").not());

    let output = fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "with typo dependency",
            "--complexity",
            "low",
            "--dependencies",
            "NOPE-1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("NOPE-1"))
        .get_output()
        .clone();
    let id = String::from_utf8(output.stdout)
        .expect("utf8 task id")
        .trim()
        .to_string();
    let shown = fixture.json(&["task", "show", &id, "--format", "json"]);
    assert_eq!(shown["dependencies"], serde_json::json!(["NOPE-1"]));
}

#[test]
fn plain_search_rows_and_empty_plugin_doctor_have_no_dangling_separators() {
    let fixture = Fixture::new();
    fixture.init_machine_and_workspace();
    fixture
        .orbit()
        .args([
            "task",
            "add",
            "--title",
            "searchable widget",
            "--complexity",
            "low",
        ])
        .assert()
        .success();

    let output = fixture
        .orbit()
        .args(["search", "widget"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("utf8 search output");
    assert!(
        stdout.contains("widget"),
        "the task should be found:\n{stdout}"
    );
    for line in stdout.lines() {
        assert!(
            !line.ends_with('\t'),
            "a plain row must not end with an empty field: {line:?}"
        );
    }

    let output = fixture
        .orbit()
        .args(["plugin", "doctor"])
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).expect("utf8 doctor output");
    assert!(
        !stdout.starts_with('\n'),
        "no rows means no separator before the summary: {stdout:?}"
    );
}
