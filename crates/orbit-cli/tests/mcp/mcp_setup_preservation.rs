//! Reconciliation and write evidence through the isolated, runtime-free CLI.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use orbit_cmd::mcp_clients::{McpClient, McpClientScope, client_config_paths};
use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let output = crate::git_repo::command()
            .arg("init")
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(output.status.success());
        fs::create_dir_all(repo.join(".orbit")).unwrap();
        Self {
            _temp: temp,
            home,
            repo,
        }
    }

    fn run(&self, args: &[&str]) {
        let mut command = cargo_bin_cmd!("orbit");
        test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env_remove("RUST_LOG")
            .args(args)
            .assert()
            .success();
    }

    fn records(&self) -> Vec<Value> {
        let path = self.home.join(".orbit/state/logs/orbit.jsonl");
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|record| record["fields"]["event"] == "mcp_config_write")
            .collect()
    }
}

fn seed(path: &Path, args: &[&str], key: &str) -> Vec<u8> {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let content = if path
        .extension()
        .is_some_and(|extension| extension == "toml")
    {
        // Force Grok's native reader when this is also its home settings file.
        format!(
            "# preserve formatting\n[claude_compat]\nimported = true\n[mcp_servers.orbit]\ncommand = '/custom/orbit'\nargs = {}\nenabled = true\n",
            serde_json::to_string(args).unwrap()
        )
    } else {
        json!({key: {"orbit": {"command": "/custom/orbit", "args": args, "env": {"CUSTOM_SETTING": "keep"}}}}).to_string()
    };
    fs::write(path, content.as_bytes()).unwrap();
    content.into_bytes()
}

#[test]
fn init_preserves_operator_launch_bytes_for_every_client_and_scope_without_a_registry() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join(".grok")).unwrap();
    fs::write(
        fixture.home.join(".grok/config.toml"),
        "[claude_compat]\nimported = true\n",
    )
    .unwrap();
    for (scope, scope_name) in [
        (McpClientScope::Workspace, "workspace"),
        (McpClientScope::Home, "home"),
    ] {
        for client in McpClient::ALL {
            let paths =
                client_config_paths(scope, client, &fixture.repo, Some(&fixture.home)).unwrap();
            for args in [
                vec!["mcp", "serve", "--operator"],
                vec!["--workspace", "ws_existing", "mcp", "serve", "--operator"],
                vec![
                    "mcp",
                    "serve",
                    "--operator",
                    "--workspace",
                    "ws_existing",
                    "--orchestrator",
                    "custom",
                ],
            ] {
                let before = seed(
                    &paths.mcp_path,
                    &args,
                    if client == McpClient::Vscode {
                        "servers"
                    } else {
                        "mcpServers"
                    },
                );
                fixture.run(&[
                    "mcp",
                    "init",
                    "--client",
                    client.label(),
                    "--scope",
                    scope_name,
                ]);
                assert_eq!(
                    fs::read(&paths.mcp_path).unwrap(),
                    before,
                    "repeat setup must preserve {client:?} {scope_name} operator registration"
                );
            }
        }
    }
    assert!(
        !orbit_registry::workspace_registry::registry_path_for(&fixture.home.join(".orbit"))
            .exists(),
        "this exercises the incident's missing-registry launch shape"
    );
    assert!(
        fixture
            .records()
            .iter()
            .all(|record| !record["fields"]["server_args"]
                .as_str()
                .unwrap()
                .contains("--operator")),
        "preserved entries must not be rewritten"
    );
}

#[test]
fn automatic_workspace_setup_preserves_operator_binding_and_remove_explicitly_resets_it() {
    let fixture = Fixture::new();
    fixture.run(&[
        "init",
        "--non-interactive",
        "--skip-host-prerequisites",
        "--machine-name",
        "mcp-preservation",
        "--task-prefix",
        "MPR",
    ]);
    fs::create_dir_all(fixture.repo.join(".claude")).unwrap();
    let path = fixture.repo.join(".mcp.json");
    let before = seed(&path, &["mcp", "serve", "--operator"], "mcpServers");
    fixture.run(&["workspace", "init", "--name", "mcp-preservation", "--mcp"]);
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "implicit setup must leave the tracked unbound operator entry unchanged"
    );
    fixture.run(&["mcp", "init", "--auto"]);
    assert_eq!(fs::read(&path).unwrap(), before);
    fixture.run(&["mcp", "remove", "--claude"]);
    fixture.run(&["mcp", "init", "--claude"]);
    let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["orbit"]["args"],
        json!(["mcp", "serve", "--workspace", "ws_mcp-preservation"])
    );
}

#[test]
fn every_actual_config_mutation_logs_path_caller_and_arguments_including_cleanup() {
    let fixture = Fixture::new();
    let legacy = fixture.repo.join(".claude.json");
    seed(&legacy, &["mcp", "serve"], "mcpServers");
    let grok_legacy = fixture.repo.join(".grok/config.toml");
    fs::create_dir_all(grok_legacy.parent().unwrap()).unwrap();
    fs::write(
        &grok_legacy,
        "[mcp_servers.orbit]\ncommand = 'orbit'\nargs = ['mcp', 'serve']\nenabled = true\n",
    )
    .unwrap();
    fixture.run(&["mcp", "init", "--all"]);
    let records = fixture.records();
    let paths = records
        .iter()
        .map(|record| PathBuf::from(record["fields"]["path"].as_str().unwrap()))
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from([legacy, grok_legacy]);
    for client in McpClient::ALL {
        let config = client_config_paths(
            McpClientScope::Workspace,
            client,
            &fixture.repo,
            Some(&fixture.home),
        )
        .unwrap();
        expected.insert(config.mcp_path);
        expected.extend(config.settings_path);
    }
    assert_eq!(
        paths, expected,
        "each actual writer, including permissions and legacy cleanup, needs durable evidence"
    );
    for record in &records {
        let fields = &record["fields"];
        assert!(fields["pid"].as_u64().unwrap() > 0);
        assert!(!fields["caller"].as_str().unwrap().is_empty());
        assert!(!fields["resolved_path"].as_str().unwrap().is_empty());
        assert!(fields["args"].as_str().unwrap().contains("--all"));
        let args: Value = serde_json::from_str(fields["server_args"].as_str().unwrap()).unwrap();
        if fields["path"] == fixture.repo.join(".mcp.json").to_str().unwrap() {
            assert_eq!(args["orbit"], json!(["mcp", "serve"]));
        }
    }
    fixture.run(&["mcp", "init", "--all"]);
    assert_eq!(
        fixture.records().len(),
        records.len(),
        "equal config content is a no-op"
    );
    fixture.run(&["mcp", "remove", "--all"]);
    let removed = fixture.records();
    assert!(removed.len() > records.len());
    assert!(
        removed[records.len()..]
            .iter()
            .all(|record| record["fields"]["action"] == "remove")
    );
}

#[test]
fn migration_retains_operator_launch_from_claude_and_shared_grok_legacy_configs() {
    for client in ["claude", "grok"] {
        let fixture = Fixture::new();
        let legacy = if client == "claude" {
            fixture.repo.join(".claude.json")
        } else {
            fixture.repo.join(".grok/config.toml")
        };
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let args = ["mcp", "serve", "--operator", "--workspace", "ws_existing"];
        if client == "claude" {
            seed(&legacy, &args, "mcpServers");
        } else {
            fs::write(
                &legacy,
                format!(
                    "[mcp_servers.orbit]\ncommand = 'orbit'\nargs = {}\nenabled = true\n",
                    serde_json::to_string(&args).unwrap()
                ),
            )
            .unwrap();
        }
        fixture.run(&["mcp", "init", "--client", client]);
        let config: Value =
            serde_json::from_slice(&fs::read(fixture.repo.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["orbit"]["args"], json!(args));
        assert!(!legacy.exists());
    }
}
