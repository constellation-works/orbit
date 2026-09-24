use tempfile::tempdir;

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::run_action;
use super::super::common::ServerLaunch;
use super::OPERATOR_LAUNCH;

#[test]
fn codex_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".codex")).expect("create .codex");
    std::fs::write(
        repo.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n[mcp_servers.other]\ncommand = \"demo\"\n",
    )
    .expect("write config");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init codex");

    let config = std::fs::read_to_string(repo.path().join(".codex").join("config.toml"))
        .expect("read config");
    let parsed: toml::Value = toml::from_str(&config).expect("parse config");
    assert_eq!(parsed["model"].as_str(), Some("gpt-5.4"));
    assert_eq!(
        parsed["mcp_servers"]["orbit"]["command"].as_str(),
        Some("orbit")
    );
    let args = parsed["mcp_servers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));
    assert!(parsed["mcp_servers"]["orbit"].get("cwd").is_none());
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove codex");

    let config = std::fs::read_to_string(repo.path().join(".codex").join("config.toml"))
        .expect("read config");
    let parsed: toml::Value = toml::from_str(&config).expect("parse config");
    assert!(
        parsed
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("orbit"))
            .is_none()
    );
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );
}

#[test]
fn workspace_scope_codex_init_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init codex");
    let first = std::fs::read_to_string(repo.path().join(".codex").join("config.toml"))
        .expect("read first config");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init codex again");
    let second = std::fs::read_to_string(repo.path().join(".codex").join("config.toml"))
        .expect("read second config");

    assert_eq!(first, second);
}

#[test]
fn codex_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init codex")
    };
    let assert_single_operator_entry = || {
        let config = std::fs::read_to_string(repo.path().join(".codex").join("config.toml"))
            .expect("read config");
        let parsed: toml::Value = toml::from_str(&config).expect("parse config");
        let args = parsed["mcp_servers"]["orbit"]["args"]
            .as_array()
            .expect("args array");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].as_str(), Some("mcp"));
        assert_eq!(args[1].as_str(), Some("serve"));
        assert_eq!(args[2].as_str(), Some("--operator"));
    };

    init();
    assert_single_operator_entry();
    init();
    assert_single_operator_entry();
}

#[test]
fn grok_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".grok")).expect("create .grok");
    std::fs::write(
        repo.path().join(".grok").join("config.toml"),
        "model = \"grok-4\"\n[mcp_servers.other]\ncommand = \"demo\"\n",
    )
    .expect("write config");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Grok]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init grok");

    let config = std::fs::read_to_string(repo.path().join(".grok").join("config.toml"))
        .expect("read config");
    let parsed: toml::Value = toml::from_str(&config).expect("parse config");
    assert_eq!(parsed["model"].as_str(), Some("grok-4"));
    assert_eq!(
        parsed["mcp_servers"]["orbit"]["command"].as_str(),
        Some("orbit")
    );
    let args = parsed["mcp_servers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));
    assert_eq!(
        parsed["mcp_servers"]["orbit"]["enabled"].as_bool(),
        Some(true)
    );
    assert!(parsed["mcp_servers"]["orbit"].get("cwd").is_none());
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Grok]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove grok");

    let config = std::fs::read_to_string(repo.path().join(".grok").join("config.toml"))
        .expect("read config");
    let parsed: toml::Value = toml::from_str(&config).expect("parse config");
    assert!(
        parsed
            .get("mcp_servers")
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("orbit"))
            .is_none()
    );
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );
}

#[test]
fn workspace_scope_grok_init_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Grok]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init grok");
    let first = std::fs::read_to_string(repo.path().join(".grok").join("config.toml"))
        .expect("read first config");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Grok]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init grok again");
    let second = std::fs::read_to_string(repo.path().join(".grok").join("config.toml"))
        .expect("read second config");

    assert_eq!(first, second);
}

#[test]
fn grok_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Grok]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init grok")
    };
    let assert_single_operator_entry = || {
        let config = std::fs::read_to_string(repo.path().join(".grok").join("config.toml"))
            .expect("read config");
        let parsed: toml::Value = toml::from_str(&config).expect("parse config");
        let args = parsed["mcp_servers"]["orbit"]["args"]
            .as_array()
            .expect("args array");
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].as_str(), Some("mcp"));
        assert_eq!(args[1].as_str(), Some("serve"));
        assert_eq!(args[2].as_str(), Some("--operator"));
    };

    init();
    assert_single_operator_entry();
    init();
    assert_single_operator_entry();
}

#[test]
fn toml_init_and_remove_preserve_user_comments_and_formatting() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let config_path = repo.path().join(".codex").join("config.toml");
    std::fs::create_dir_all(config_path.parent().expect("parent")).expect("create .codex");
    let original = "# my model choice\nmodel = \"gpt-5.4\" # pinned\n";
    std::fs::write(&config_path, original).expect("write config");
    let run = |action| {
        run_action(
            action,
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Codex]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("run codex action")
    };

    run(McpAction::Init(ServerLaunch::default()));
    let config = std::fs::read_to_string(&config_path).expect("read config");
    assert!(
        config.starts_with(original),
        "user lines survive init: {config}"
    );
    assert!(config.contains("[mcp_servers.orbit]"), "{config}");

    run(McpAction::Remove);
    assert_eq!(
        std::fs::read_to_string(&config_path).expect("read config"),
        original,
        "remove restores the user's file byte-for-byte"
    );
}
