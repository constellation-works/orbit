use tempfile::tempdir;

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::run_action;
use super::super::common::ServerLaunch;
use super::OPERATOR_LAUNCH;

#[test]
fn antigravity_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".agents")).expect("create .agents");
    std::fs::write(
        repo.path().join(".agents").join("mcp_config.json"),
        "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write mcp_config");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Antigravity]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init antigravity");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".agents").join("mcp_config.json"))
            .expect("read mcp_config"),
    )
    .expect("parse mcp_config");
    assert!(settings["mcpServers"]["orbit"].is_object());
    assert!(settings["mcpServers"]["other"].is_object());

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Antigravity]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove antigravity");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".agents").join("mcp_config.json"))
            .expect("read mcp_config"),
    )
    .expect("parse mcp_config");
    assert!(settings["mcpServers"]["orbit"].is_null());
    assert!(settings["mcpServers"]["other"].is_object());
}

#[test]
fn antigravity_home_scope_writes_official_mcp_config_path() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(OPERATOR_LAUNCH),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Antigravity]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init antigravity home");

    let path = home
        .path()
        .join(".gemini")
        .join("config")
        .join("mcp_config.json");
    assert!(path.is_file(), "home MCP config {}", path.display());
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("parse");
    assert!(settings["mcpServers"]["orbit"].is_object());
}
