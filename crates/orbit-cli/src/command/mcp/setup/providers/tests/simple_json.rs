use serde_json::json;
use tempfile::tempdir;

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::{auto_detected_providers, run_action, vscode_home_user_dir};
use super::super::claude::claude_mcp_server_value;
use super::super::common::ServerLaunch;
use super::super::simple_json::simple_mcp_server_value;
use super::super::toml_servers::mcp_server_table;
use super::{BOUND_LAUNCH, OPERATOR_LAUNCH};

#[test]
fn server_value_builders_emit_mcp_serve_only() {
    let claude = claude_mcp_server_value(ServerLaunch::default());
    let claude_args = claude["args"].as_array().expect("claude args");
    assert_eq!(claude_args.len(), 2);
    assert_eq!(claude_args[0].as_str(), Some("mcp"));
    assert_eq!(claude_args[1].as_str(), Some("serve"));

    let gemini = simple_mcp_server_value(ServerLaunch::default());
    let gemini_args = gemini["args"].as_array().expect("gemini args");
    assert_eq!(gemini_args.len(), 2);
    assert!(gemini.get("cwd").is_none());

    let codex = mcp_server_table(ServerLaunch::default());
    let codex_args = codex["args"].as_array().expect("codex args");
    assert_eq!(codex_args.len(), 2);
    assert!(codex.get("cwd").is_none());
    assert_eq!(codex["enabled"].as_bool(), Some(true));
}

#[test]
fn server_value_builders_emit_federated_mode_only_when_requested() {
    let expected = ["mcp", "serve", "--mode", "federated"];

    for (provider, args) in [
        (
            "claude",
            json_args(&claude_mcp_server_value(ServerLaunch::Federated)),
        ),
        (
            "gemini",
            json_args(&simple_mcp_server_value(ServerLaunch::Federated)),
        ),
        (
            "codex",
            toml_args(&mcp_server_table(ServerLaunch::Federated)),
        ),
        (
            "grok",
            toml_args(&mcp_server_table(ServerLaunch::Federated)),
        ),
    ] {
        assert_eq!(args, expected, "{provider} must launch the federated mux");
    }
}

#[test]
fn server_value_builders_append_operator_flag_when_authorized() {
    let claude = claude_mcp_server_value(OPERATOR_LAUNCH);
    let claude_args = claude["args"].as_array().expect("claude args");
    assert_eq!(claude_args.len(), 3);
    assert_eq!(claude_args[0].as_str(), Some("mcp"));
    assert_eq!(claude_args[1].as_str(), Some("serve"));
    assert_eq!(claude_args[2].as_str(), Some("--operator"));

    let gemini = simple_mcp_server_value(OPERATOR_LAUNCH);
    let gemini_args = gemini["args"].as_array().expect("gemini args");
    assert_eq!(
        gemini_args,
        &vec![json!("mcp"), json!("serve"), json!("--operator")]
    );

    let codex = mcp_server_table(OPERATOR_LAUNCH);
    let codex_args = codex["args"].as_array().expect("codex args");
    assert_eq!(codex_args.len(), 3);
    assert_eq!(
        codex_args.get(2).and_then(|arg| arg.as_str()),
        Some("--operator")
    );
    assert_eq!(codex["enabled"].as_bool(), Some(true));
}

/// A generated integration is written for one registered workspace, so its
/// argv names that workspace. Without it a client that cannot announce
/// `_meta.orbit.workspace` at initialize would have to repeat the selector on
/// every workspace-scoped call.
#[test]
fn server_value_builders_bind_the_registered_workspace() {
    let expected = ["mcp", "serve", "--workspace", "ws_demo"];

    for (provider, args) in [
        ("claude", json_args(&claude_mcp_server_value(BOUND_LAUNCH))),
        ("gemini", json_args(&simple_mcp_server_value(BOUND_LAUNCH))),
        ("codex", toml_args(&mcp_server_table(BOUND_LAUNCH))),
        ("grok", toml_args(&mcp_server_table(BOUND_LAUNCH))),
    ] {
        assert_eq!(args, expected, "{provider} must launch bound");
    }
}

fn json_args(server: &serde_json::Value) -> Vec<String> {
    server["args"]
        .as_array()
        .expect("json args")
        .iter()
        .map(|arg| arg.as_str().expect("string arg").to_string())
        .collect()
}

fn toml_args(server: &toml_edit::Table) -> Vec<String> {
    server["args"]
        .as_array()
        .expect("toml args")
        .iter()
        .map(|arg| arg.as_str().expect("string arg").to_string())
        .collect()
}

/// Authority and binding are independent: the bootstrap path emits both.
#[test]
fn server_value_builders_emit_authority_and_binding_together() {
    let launch = ServerLaunch::local(true, Some("ws_demo"));
    assert_eq!(
        json_args(&claude_mcp_server_value(launch)),
        ["mcp", "serve", "--operator", "--workspace", "ws_demo"]
    );
}

#[test]
fn cursor_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".cursor")).expect("create .cursor");
    std::fs::write(
        repo.path().join(".cursor").join("mcp.json"),
        "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write cursor mcp file");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init cursor");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".cursor").join("mcp.json"))
            .expect("read cursor mcp"),
    )
    .expect("parse cursor mcp");
    assert!(mcp["mcpServers"]["orbit"].is_object());
    assert!(mcp["mcpServers"]["other"].is_object());
    assert!(mcp["mcpServers"]["orbit"]["cwd"].is_null());
    let args = mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove cursor");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".cursor").join("mcp.json"))
            .expect("read cursor mcp"),
    )
    .expect("parse cursor mcp");
    assert!(mcp["mcpServers"]["orbit"].is_null());
    assert!(mcp["mcpServers"]["other"].is_object());
}

#[test]
fn cursor_home_scope_init_writes_resolved_home_path() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init cursor home");

    let cursor_path = home.path().join(".cursor").join("mcp.json");
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cursor_path).expect("read cursor home mcp"))
            .expect("parse cursor home mcp");
    let args = mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("cursor args");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));
    assert!(mcp["mcpServers"]["orbit"]["cwd"].is_null());
    assert!(!repo.path().join(".cursor").join("mcp.json").exists());
}

#[test]
fn auto_detects_cursor_from_repo_marker() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".cursor")).expect("create .cursor");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert!(providers.contains(&McpProvider::Cursor));
}

#[test]
fn auto_detects_cursor_from_home_marker() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".cursor")).expect("create cursor home dir");
    std::fs::write(home.path().join(".cursor").join("mcp.json"), "{}\n")
        .expect("write cursor home file");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert!(providers.contains(&McpProvider::Cursor));
}

#[test]
fn workspace_scope_cursor_init_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init cursor");
    let first = std::fs::read_to_string(repo.path().join(".cursor").join("mcp.json"))
        .expect("read first cursor");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init cursor again");
    let second = std::fs::read_to_string(repo.path().join(".cursor").join("mcp.json"))
        .expect("read second cursor");

    assert_eq!(first, second);
}

#[test]
fn vscode_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".vscode")).expect("create .vscode");
    std::fs::write(
        repo.path().join(".vscode").join("mcp.json"),
        "{\n  \"servers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write vscode mcp file");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init vscode");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".vscode").join("mcp.json"))
            .expect("read vscode mcp"),
    )
    .expect("parse vscode mcp");
    assert!(mcp["servers"]["orbit"].is_object());
    assert!(mcp["servers"]["other"].is_object());
    assert!(
        mcp.get("mcpServers").is_none(),
        "vscode must not write a mcpServers key"
    );
    assert!(mcp["servers"]["orbit"]["cwd"].is_null());
    let args = mcp["servers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove vscode");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".vscode").join("mcp.json"))
            .expect("read vscode mcp"),
    )
    .expect("parse vscode mcp");
    assert!(mcp["servers"]["orbit"].is_null());
    assert!(mcp["servers"]["other"].is_object());
}

#[test]
fn vscode_home_scope_init_writes_resolved_home_path() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init vscode home");

    let resolved = vscode_home_user_dir(home.path()).join("mcp.json");
    assert!(
        resolved.is_file(),
        "expected vscode home mcp at {}",
        resolved.display()
    );
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&resolved).expect("read vscode home"))
            .expect("parse vscode home");
    let args = mcp["servers"]["orbit"]["args"]
        .as_array()
        .expect("vscode args");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));
    assert!(mcp.get("mcpServers").is_none());
    assert!(mcp["servers"]["orbit"]["cwd"].is_null());
    assert!(!repo.path().join(".vscode").join("mcp.json").exists());
}

#[test]
fn auto_detects_vscode_from_repo_marker() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".vscode")).expect("create .vscode");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert!(providers.contains(&McpProvider::Vscode));
}

#[test]
fn auto_detects_vscode_from_home_marker() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let user_dir = vscode_home_user_dir(home.path());
    std::fs::create_dir_all(&user_dir).expect("create vscode user dir");
    std::fs::write(user_dir.join("mcp.json"), "{}\n").expect("write vscode home mcp");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert!(providers.contains(&McpProvider::Vscode));
}

#[test]
fn workspace_scope_vscode_init_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init vscode");
    let first = std::fs::read_to_string(repo.path().join(".vscode").join("mcp.json"))
        .expect("read first vscode");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init vscode again");
    let second = std::fs::read_to_string(repo.path().join(".vscode").join("mcp.json"))
        .expect("read second vscode");

    assert_eq!(first, second);
}

#[test]
fn windsurf_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let windsurf_dir = repo.path().join(".codeium").join("windsurf");
    std::fs::create_dir_all(&windsurf_dir).expect("create windsurf dir");
    std::fs::write(
        windsurf_dir.join("mcp_config.json"),
        "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write windsurf config");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init windsurf");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(windsurf_dir.join("mcp_config.json"))
            .expect("read windsurf config"),
    )
    .expect("parse windsurf config");
    assert!(mcp["mcpServers"]["orbit"].is_object());
    assert!(mcp["mcpServers"]["other"].is_object());
    assert!(mcp["mcpServers"]["orbit"]["cwd"].is_null());
    let args = mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove windsurf");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(windsurf_dir.join("mcp_config.json"))
            .expect("read windsurf config"),
    )
    .expect("parse windsurf config");
    assert!(mcp["mcpServers"]["orbit"].is_null());
    assert!(mcp["mcpServers"]["other"].is_object());
}

#[test]
fn windsurf_home_scope_init_writes_resolved_home_path() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init windsurf home");

    let path = home
        .path()
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read windsurf home"))
            .expect("parse windsurf home");
    let args = mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("windsurf args");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));
    assert!(mcp["mcpServers"]["orbit"]["cwd"].is_null());
    assert!(
        !repo
            .path()
            .join(".codeium")
            .join("windsurf")
            .join("mcp_config.json")
            .exists()
    );
}

#[test]
fn auto_detects_windsurf_from_home_marker() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let windsurf_dir = home.path().join(".codeium").join("windsurf");
    std::fs::create_dir_all(&windsurf_dir).expect("create windsurf home dir");
    std::fs::write(windsurf_dir.join("mcp_config.json"), "{}\n").expect("write windsurf home file");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert!(providers.contains(&McpProvider::Windsurf));
}

#[test]
fn workspace_scope_windsurf_init_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init windsurf");
    let path = repo
        .path()
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");
    let first = std::fs::read_to_string(&path).expect("read first windsurf");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init windsurf again");
    let second = std::fs::read_to_string(&path).expect("read second windsurf");

    assert_eq!(first, second);
}

/// Shared assertion for the simple-JSON providers (cursor, vscode, windsurf):
/// the written `orbit` server entry's `args` array is exactly `["mcp",
/// "serve", "--operator"]` under the given top-level servers key.
fn assert_single_operator_entry(config_path: &std::path::Path, top_level_key: &str) {
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(config_path).expect("read config"))
            .expect("parse config");
    let args = config[top_level_key]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(
        args,
        &vec![
            serde_json::json!("mcp"),
            serde_json::json!("serve"),
            serde_json::json!("--operator"),
        ]
    );
}

#[test]
fn cursor_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let path = repo.path().join(".cursor").join("mcp.json");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Cursor]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init cursor")
    };

    init();
    assert_single_operator_entry(&path, "mcpServers");
    init();
    assert_single_operator_entry(&path, "mcpServers");
}

#[test]
fn vscode_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let path = repo.path().join(".vscode").join("mcp.json");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Vscode]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init vscode")
    };

    init();
    assert_single_operator_entry(&path, "servers");
    init();
    assert_single_operator_entry(&path, "servers");
}

#[test]
fn windsurf_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let path = repo
        .path()
        .join(".codeium")
        .join("windsurf")
        .join("mcp_config.json");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Windsurf]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init windsurf")
    };

    init();
    assert_single_operator_entry(&path, "mcpServers");
    init();
    assert_single_operator_entry(&path, "mcpServers");
}
