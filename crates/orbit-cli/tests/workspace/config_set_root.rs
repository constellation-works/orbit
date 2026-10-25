//! `orbit --root <dir> config set` with no distinct workspace layer.
//!
//! `--root` pins one config root for both layers, so a bare `config set` has
//! no workspace file to edit. It must refuse with a message that names
//! `--global` and the file that flag edits, and leave every config file
//! untouched; `--global` against the same root must then apply the edit.

#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

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
        Self::initialized(true)
    }

    fn layered() -> Self {
        Self::initialized(false)
    }

    fn initialized(override_root: bool) -> Self {
        let temp = tempdir().expect("tempdir");
        let root = if override_root {
            temp.path().join("scratch-root")
        } else {
            temp.path().join("home/.orbit")
        };
        let fixture = Self {
            home: temp.path().join("home"),
            repo: temp.path().join("repo"),
            root,
            _temp: temp,
        };
        fs::create_dir_all(&fixture.home).expect("home");
        fs::create_dir_all(&fixture.repo).expect("repo");
        crate::git_repo::SeededRepo::new(&fixture.repo)
            .branch("agent-main")
            .identity("test", "test@example.invalid")
            .empty_commit("init")
            .build();
        let root = fixture.root.to_string_lossy().into_owned();
        let prefix = if override_root {
            vec!["--root", &root]
        } else {
            vec![]
        };
        let mut args = prefix.clone();
        args.extend([
            "init",
            "--non-interactive",
            "--machine-name",
            "config-root-host",
            "--task-prefix",
            "CR",
        ]);
        fixture.orbit(&args).success();
        let mut args = prefix;
        args.extend(["workspace", "init"]);
        fixture.orbit(&args).success();
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

/// Literal syntax, file preservation, init modes and confirmation output are
/// local CLI contracts even though the write transaction is shared with HTTP.
#[test]
fn config_set_preserves_bytes_and_confirmations_in_each_mode() {
    let fixture = Fixture::layered();
    let workspace = fixture.repo.join(".orbit/config.toml");
    let mut identity = fs::read_to_string(fixture.global_config())
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let keys = identity
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    for key in keys {
        if key != "machine" {
            identity.remove(&key);
        }
    }
    let policy =
        "[workflow]\nbase_branch = 'before' # keep comment\n\n[execution.env]\npass = ['TOKEN']\n";
    let global = format!("{identity}{policy}");
    for (mode, flags) in [
        ("existing", vec![]),
        ("seed", vec!["--seed-from-global"]),
        ("fresh", vec!["--fresh"]),
        ("global", vec!["--global"]),
    ] {
        for format in ["plain", "json"] {
            fs::write(fixture.global_config(), &global).unwrap();
            if workspace.exists() {
                fs::remove_file(&workspace).unwrap();
            }
            if mode == "existing" {
                fs::write(&workspace, policy).unwrap();
            }
            let mut args = vec!["--format", format, "config", "set"];
            args.extend(&flags);
            args.extend(["workflow.base_branch", "feature branch"]);
            let output = fixture.orbit(&args).success().get_output().stdout.clone();
            let (scope, path, before) = if mode == "global" {
                ("global", fixture.global_config(), global.as_str())
            } else {
                (
                    "workspace",
                    workspace.clone(),
                    if mode == "fresh" { "" } else { policy },
                )
            };
            let expected = if before.is_empty() {
                "[workflow]\nbase_branch = \"feature branch\"\n".to_string()
            } else {
                before.replace("'before' # keep comment", "\"feature branch\"")
            };
            let expected = if mode == "seed" {
                format!("\n{expected}")
            } else {
                expected
            };
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                expected,
                "{mode}/{format}"
            );
            let path = if scope == "workspace" {
                fs::canonicalize(&path).unwrap()
            } else {
                path
            };
            if format == "json" {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&output).unwrap(),
                    serde_json::json!({"key":"workflow.base_branch", "scope":scope, "path":path})
                );
            } else {
                assert_eq!(
                    String::from_utf8(output).unwrap(),
                    format!(
                        "set workflow.base_branch ({scope} config: {})\n",
                        path.display()
                    )
                );
            }
            if scope == "workspace" {
                assert_eq!(fs::read_to_string(fixture.global_config()).unwrap(), global);
            }
        }
    }
    fixture
        .orbit(&[
            "config",
            "set",
            "--fresh",
            "execution.env.pass",
            "['A', 'B']",
        ])
        .success();
    let parsed: toml::Value = toml::from_str(&fs::read_to_string(&workspace).unwrap()).unwrap();
    assert_eq!(
        parsed["execution"]["env"]["pass"].as_array().unwrap(),
        &vec![
            toml::Value::String("A".into()),
            toml::Value::String("B".into())
        ]
    );
}

#[test]
fn config_set_refusals_match_shared_admission_without_writes() {
    if crate::isolated_test::run_in_child(
        "config_set_root::config_set_refusals_match_shared_admission_without_writes",
    ) {
        return;
    }
    let fixture = Fixture::layered();
    let workspace = fixture.repo.join(".orbit/config.toml");
    // Exercise first-write admission without the policy workspace init now seeds.
    fs::remove_file(&workspace).expect("remove fixture workspace policy");
    let unknown = orbit_config::admit_config_key("workflow.base_brunch")
        .unwrap_err()
        .to_string();
    let refused = fixture
        .orbit(&["config", "set", "workflow.base_brunch", "main"])
        .failure();
    assert!(String::from_utf8_lossy(&refused.get_output().stderr).contains(&unknown));
    assert!(
        !workspace.exists(),
        "unknown-key refusal precedes init admission"
    );
    fixture
        .orbit(&["config", "set", "workflow.base_branch", "main"])
        .failure();
    assert!(
        !workspace.exists(),
        "require-existing never creates a policy layer"
    );
    let mut global = fs::read_to_string(fixture.global_config())
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    global["workflow"]["resource_throttle"]["cpu_resume_percent"] = toml_edit::value(70);
    fs::write(fixture.global_config(), global.to_string()).unwrap();
    fs::write(
        &workspace,
        "[workflow.resource_throttle]\ncpu_high_percent = 80\n",
    )
    .unwrap();
    let runtime =
        orbit_core::OrbitRuntime::from_roots(&fixture.root, workspace.parent().unwrap()).unwrap();
    for (key, value, flags, typed) in [
        (
            "workflow.base_brunch",
            "main",
            vec![],
            serde_json::json!("main"),
        ),
        (
            "workflow.resource_throttle.cpu_resume_percent",
            "85",
            vec!["--global"],
            serde_json::json!(85),
        ),
    ] {
        let before = (
            fs::read(fixture.global_config()).unwrap(),
            fs::read(&workspace).unwrap(),
        );
        let scope = if flags.is_empty() {
            orbit_config::ConfigScope::Workspace
        } else {
            orbit_config::ConfigScope::Global
        };
        let expected = orbit_core::application::config::set_key(
            &runtime,
            key,
            &typed,
            scope,
            orbit_core::application::config::ConfigWriteInit::RequireExisting,
        )
        .unwrap_err()
        .to_string();
        let mut args = vec!["config", "set"];
        args.extend(flags);
        args.extend([key, value]);
        let output = fixture.orbit(&args).failure().get_output().stderr.clone();
        assert!(
            String::from_utf8_lossy(&output).contains(&expected),
            "{}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(
            (
                fs::read(fixture.global_config()).unwrap(),
                fs::read(&workspace).unwrap()
            ),
            before
        );
    }
    let events = runtime
        .list_audit_events_filtered(&orbit_core::AuditEventFilter::default())
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.command == "config" && event.subcommand.as_deref() == Some("set")),
        "CLI config writes keep their persistent invocation audit"
    );
    // Global has no workspace init choice; its conflicting flags stay rejected.
    for flag in ["--fresh", "--seed-from-global"] {
        fixture
            .orbit(&[
                "config",
                "set",
                "--global",
                flag,
                "workflow.base_branch",
                "main",
            ])
            .failure();
    }
}
