use std::path::Path;

use tempfile::tempdir;

use super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::dispatch::{
    auto_detected_providers, format_action_summary, run_action, vscode_home_user_dir,
};
use super::super::providers::ServerLaunch;

#[test]
fn auto_detects_expected_providers() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("create .claude");
    std::fs::create_dir_all(repo.path().join(".gemini")).expect("create .gemini");
    std::fs::create_dir_all(repo.path().join(".grok")).expect("create .grok");
    std::fs::create_dir_all(home.path().join(".codex")).expect("create codex dir");
    std::fs::write(
        home.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n",
    )
    .expect("write global codex config");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert_eq!(
        providers,
        vec![
            McpProvider::Claude,
            McpProvider::Codex,
            McpProvider::Gemini,
            McpProvider::Grok,
        ]
    );
}

#[test]
fn auto_detects_gemini_from_home_when_repo_lacks_dotgemini() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".gemini")).expect("create gemini home dir");
    std::fs::write(home.path().join(".gemini").join("settings.json"), "{}\n")
        .expect("write global gemini settings");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert_eq!(providers, vec![McpProvider::Gemini]);
}

#[test]
fn auto_detects_grok_from_home_when_repo_lacks_dotgrok() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".grok")).expect("create grok home dir");
    std::fs::write(home.path().join(".grok").join("config.toml"), "\n")
        .expect("write global grok config");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));
    assert_eq!(providers, vec![McpProvider::Grok]);
}

#[test]
fn auto_detects_claude_from_home_when_repo_lacks_dotclaude() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::write(home.path().join(".claude.json"), "{}\n").expect("write claude home config");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));

    assert_eq!(providers, vec![McpProvider::Claude]);
}

#[test]
fn auto_detects_claude_from_home_settings_when_repo_lacks_dotclaude() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".claude")).expect("create claude home dir");
    std::fs::write(home.path().join(".claude").join("settings.json"), "{}\n")
        .expect("write claude home settings");

    let providers = auto_detected_providers(repo.path(), Some(home.path()));

    assert_eq!(providers, vec![McpProvider::Claude]);
}

#[test]
fn orbit_own_home_skill_links_do_not_auto_detect_claude() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    // Exactly what `orbit init` leaves behind in a home that has never run
    // Claude Code: skill-link directories and nothing else.
    for provider_dir in [".claude", ".agents"] {
        std::fs::create_dir_all(home.path().join(provider_dir).join("skills"))
            .expect("create orbit skill link dir");
    }

    let providers = auto_detected_providers(repo.path(), Some(home.path()));

    assert!(
        !providers.contains(&McpProvider::Claude),
        "orbit-created ~/.claude/skills must not count as a Claude install: {providers:?}"
    );
}

#[test]
fn home_scope_writes_to_home_paths_and_skips_repo_files() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![
            McpProvider::Claude,
            McpProvider::Codex,
            McpProvider::Gemini,
            McpProvider::Grok,
        ]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init home scope");

    let claude_mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json")).expect("read claude home mcp"),
    )
    .expect("parse claude mcp");
    let claude_args = claude_mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("claude args");
    assert_eq!(claude_args.len(), 2);
    assert_eq!(claude_args[0].as_str(), Some("mcp"));
    assert_eq!(claude_args[1].as_str(), Some("serve"));
    assert!(claude_mcp["mcpServers"]["orbit"]["cwd"].is_null());

    let claude_settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude").join("settings.json"))
            .expect("read claude home settings"),
    )
    .expect("parse claude settings");
    let allow = claude_settings["permissions"]["allow"]
        .as_array()
        .expect("allow array");
    assert!(
        allow
            .iter()
            .any(|item| item == "mcp__orbit__orbit_task_show")
    );
    assert!(
        !allow
            .iter()
            .any(|item| item.as_str().is_some_and(|s| s.starts_with("mcp__plugin_"))),
        "CLI init must not emit Claude Code plugin-scoped permission names; \
         that shape is synthesized by Claude itself for plugin installs",
    );

    let codex_config = std::fs::read_to_string(home.path().join(".codex").join("config.toml"))
        .expect("read codex home config");
    let codex_parsed: toml::Value = toml::from_str(&codex_config).expect("parse codex");
    let codex_args = codex_parsed["mcp_servers"]["orbit"]["args"]
        .as_array()
        .expect("codex args");
    assert_eq!(codex_args.len(), 2);
    assert_eq!(codex_args[0].as_str(), Some("mcp"));
    assert_eq!(codex_args[1].as_str(), Some("serve"));
    assert!(codex_parsed["mcp_servers"]["orbit"].get("cwd").is_none());

    let gemini_settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".gemini").join("settings.json"))
            .expect("read gemini home settings"),
    )
    .expect("parse gemini");
    let gemini_args = gemini_settings["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("gemini args");
    assert_eq!(gemini_args.len(), 2);
    assert!(gemini_settings["mcpServers"]["orbit"]["cwd"].is_null());

    let grok_config = std::fs::read_to_string(home.path().join(".grok").join("config.toml"))
        .expect("read grok home config");
    let grok_parsed: toml::Value = toml::from_str(&grok_config).expect("parse grok");
    let grok_args = grok_parsed["mcp_servers"]["orbit"]["args"]
        .as_array()
        .expect("grok args");
    assert_eq!(grok_args.len(), 2);
    assert_eq!(grok_args[0].as_str(), Some("mcp"));
    assert_eq!(grok_args[1].as_str(), Some("serve"));
    assert_eq!(
        grok_parsed["mcp_servers"]["orbit"]["enabled"].as_bool(),
        Some(true)
    );
    assert!(grok_parsed["mcp_servers"]["orbit"].get("cwd").is_none());

    // Repo-local files should not have been touched.
    assert!(!repo.path().join(".mcp.json").exists());
    assert!(!repo.path().join(".claude.json").exists());
    assert!(!repo.path().join(".codex").join("config.toml").exists());
    assert!(!repo.path().join(".gemini").join("settings.json").exists());
    assert!(!repo.path().join(".grok").join("config.toml").exists());
    assert!(!repo.path().join(".claude").join("settings.json").exists());
}

#[test]
fn federated_home_scope_preserves_v1_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let providers = vec![
        McpProvider::Claude,
        McpProvider::Codex,
        McpProvider::Gemini,
        McpProvider::Grok,
    ];
    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(providers.clone()),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init v1 home scope");
    run_action(
        McpAction::Init(ServerLaunch::Federated),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(providers.clone()),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init federated home scope");

    let claude: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json")).expect("read claude mcp"),
    )
    .expect("parse claude mcp");
    assert_eq!(
        claude["mcpServers"]["orbit"]["args"],
        serde_json::json!(["mcp", "serve"])
    );
    assert_eq!(
        claude["mcpServers"]["orbit-federated"]["args"],
        serde_json::json!(["mcp", "serve", "--mode", "federated"])
    );
    let claude_settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude").join("settings.json"))
            .expect("read claude settings"),
    )
    .expect("parse claude settings");
    assert!(
        claude_settings["permissions"]["allow"]
            .as_array()
            .expect("claude allow list")
            .iter()
            .any(|permission| permission == "mcp__orbit-federated__orbit_task_show")
    );

    for (provider, path) in [
        ("codex", home.path().join(".codex").join("config.toml")),
        ("grok", home.path().join(".grok").join("config.toml")),
    ] {
        let config: toml::Value = toml::from_str(
            &std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("read {provider}: {error}")),
        )
        .unwrap_or_else(|error| panic!("parse {provider}: {error}"));
        assert_eq!(
            config["mcp_servers"]["orbit"]["args"]
                .as_array()
                .map(Vec::len),
            Some(2),
            "{provider} must retain v1"
        );
        assert_eq!(
            config["mcp_servers"]["orbit-federated"]["args"]
                .as_array()
                .map(Vec::len),
            Some(4),
            "{provider} must add federated"
        );
    }

    let gemini: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".gemini").join("settings.json"))
            .expect("read gemini settings"),
    )
    .expect("parse gemini settings");
    assert_eq!(
        gemini["mcpServers"]["orbit"]["args"],
        serde_json::json!(["mcp", "serve"])
    );
    assert_eq!(
        gemini["mcpServers"]["orbit-federated"]["args"],
        serde_json::json!(["mcp", "serve", "--mode", "federated"])
    );

    run_action(
        McpAction::RemoveFederated,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(providers),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("remove federated home scope");

    let claude: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json"))
            .expect("read claude after remove"),
    )
    .expect("parse claude after remove");
    assert!(claude["mcpServers"]["orbit"].is_object());
    assert!(claude["mcpServers"]["orbit-federated"].is_null());

    let codex: toml::Value = toml::from_str(
        &std::fs::read_to_string(home.path().join(".codex").join("config.toml"))
            .expect("read codex after remove"),
    )
    .expect("parse codex after remove");
    assert!(codex["mcp_servers"]["orbit"].is_table());
    assert!(codex["mcp_servers"].get("orbit-federated").is_none());

    let gemini: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".gemini").join("settings.json"))
            .expect("read gemini after remove"),
    )
    .expect("parse gemini after remove");
    assert!(gemini["mcpServers"]["orbit"].is_object());
    assert!(gemini["mcpServers"]["orbit-federated"].is_null());

    let grok: toml::Value = toml::from_str(
        &std::fs::read_to_string(home.path().join(".grok").join("config.toml"))
            .expect("read grok after remove"),
    )
    .expect("parse grok after remove");
    assert!(grok["mcp_servers"]["orbit"].is_table());
    assert!(grok["mcp_servers"].get("orbit-federated").is_none());
}

#[test]
fn home_scope_remove_strips_only_orbit_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".codex")).expect("create codex home");
    std::fs::write(
        home.path().join(".codex").join("config.toml"),
        "model = \"gpt-5.4\"\n[mcp_servers.other]\ncommand = \"demo\"\n",
    )
    .expect("write codex config");
    std::fs::create_dir_all(home.path().join(".gemini")).expect("create gemini home");
    std::fs::write(
        home.path().join(".gemini").join("settings.json"),
        "{\n  \"theme\": \"dark\",\n  \"mcpServers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write gemini settings");
    std::fs::create_dir_all(home.path().join(".grok")).expect("create grok home");
    std::fs::write(
        home.path().join(".grok").join("config.toml"),
        "model = \"grok-4\"\n[mcp_servers.other]\ncommand = \"demo\"\n",
    )
    .expect("write grok config");

    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![
            McpProvider::Codex,
            McpProvider::Gemini,
            McpProvider::Grok,
        ]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init home scope");

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![
            McpProvider::Codex,
            McpProvider::Gemini,
            McpProvider::Grok,
        ]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("remove home scope");

    let codex_config = std::fs::read_to_string(home.path().join(".codex").join("config.toml"))
        .expect("read codex");
    let codex_parsed: toml::Value = toml::from_str(&codex_config).expect("parse codex");
    assert_eq!(codex_parsed["model"].as_str(), Some("gpt-5.4"));
    assert_eq!(
        codex_parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );
    assert!(
        codex_parsed["mcp_servers"]
            .as_table()
            .and_then(|t| t.get("orbit"))
            .is_none()
    );

    let gemini_settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".gemini").join("settings.json"))
            .expect("read gemini"),
    )
    .expect("parse gemini");
    assert_eq!(gemini_settings["theme"], "dark");
    assert!(gemini_settings["mcpServers"]["orbit"].is_null());
    assert!(gemini_settings["mcpServers"]["other"].is_object());

    let grok_config =
        std::fs::read_to_string(home.path().join(".grok").join("config.toml")).expect("read grok");
    let grok_parsed: toml::Value = toml::from_str(&grok_config).expect("parse grok");
    assert_eq!(grok_parsed["model"].as_str(), Some("grok-4"));
    assert_eq!(
        grok_parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("demo")
    );
    assert!(
        grok_parsed["mcp_servers"]
            .as_table()
            .and_then(|t| t.get("orbit"))
            .is_none()
    );
}

#[test]
fn home_scope_without_home_dir_errors() {
    let repo = tempdir().expect("repo tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let err = run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        None,
        ScopeArg::Home,
    )
    .expect_err("home scope without home dir should fail");

    assert!(matches!(
        err,
        orbit_core::OrbitError::InvalidInput(message) if message.contains("HOME")
    ));
}

#[test]
fn action_summary_names_the_resolved_checkout_and_workspace_id() {
    let summary = format_action_summary(
        McpAction::Init(ServerLaunch::default()),
        &[McpProvider::Claude],
        Path::new("/tmp/qa/repoA"),
        None,
        ScopeArg::Workspace,
        Some("ws_repoa"),
    )
    .expect("workspace scope summary");
    assert_eq!(summary, "mcp init: claude -> /tmp/qa/repoA (ws_repoa)");
}

#[test]
fn action_summary_omits_workspace_id_when_unregistered() {
    let summary = format_action_summary(
        McpAction::Remove,
        &[McpProvider::Claude, McpProvider::Codex],
        Path::new("/tmp/qa/repoA"),
        None,
        ScopeArg::Workspace,
        None,
    )
    .expect("workspace scope summary");
    assert_eq!(summary, "mcp remove: claude, codex -> /tmp/qa/repoA");
}

#[test]
fn action_summary_skips_path_when_no_providers_selected() {
    let summary = format_action_summary(
        McpAction::Init(ServerLaunch::default()),
        &[],
        Path::new("/tmp/qa/repoA"),
        None,
        ScopeArg::Workspace,
        Some("ws_repoa"),
    )
    .expect("empty provider summary");
    assert_eq!(summary, "mcp init: no providers selected");
}

#[test]
fn action_summary_names_the_resolved_home_scope_file_and_workspace_id() {
    let home = tempdir().expect("home tempdir");
    let summary = format_action_summary(
        McpAction::Init(ServerLaunch::default()),
        &[McpProvider::Claude],
        Path::new("/tmp/qa/repoA"),
        Some(home.path()),
        ScopeArg::Home,
        Some("ws_wshome"),
    )
    .expect("home scope summary");
    assert_eq!(
        summary,
        format!(
            "mcp init: claude -> {} (bound to ws_wshome)",
            home.path().join(".claude.json").display()
        )
    );
}

#[test]
fn action_summary_omits_workspace_id_for_home_scope_when_unregistered() {
    let home = tempdir().expect("home tempdir");
    let summary = format_action_summary(
        McpAction::Remove,
        &[McpProvider::Claude, McpProvider::Codex],
        Path::new("/tmp/qa/repoA"),
        Some(home.path()),
        ScopeArg::Home,
        None,
    )
    .expect("home scope summary");
    assert_eq!(
        summary,
        format!(
            "mcp remove: claude -> {}, codex -> {}",
            home.path().join(".claude.json").display(),
            home.path().join(".codex").join("config.toml").display()
        )
    );
}

#[test]
fn vscode_home_user_dir_resolves_for_host_platform() {
    let home = std::path::PathBuf::from("/tmp/orbit-test-home");
    let resolved = vscode_home_user_dir(&home);
    // Tail must always be `Code/User`; the rest is platform-specific.
    let mut components = resolved
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let user = components.pop().expect("user segment");
    let code = components.pop().expect("code segment");
    assert_eq!(user, "User");
    assert_eq!(code, "Code");
    assert!(
        resolved.starts_with(&home),
        "resolved path {} should start with home dir {}",
        resolved.display(),
        home.display(),
    );
}
