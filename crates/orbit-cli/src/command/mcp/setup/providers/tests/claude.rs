use tempfile::tempdir;

use super::super::super::args::{McpAction, McpProvider, ProviderSelectionMode, ScopeArg};
use super::super::super::dispatch::run_action;
use super::super::claude::*;
use super::super::common::ServerLaunch;
use super::OPERATOR_LAUNCH;

#[test]
fn claude_workspace_scope_init_and_remove_preserve_unrelated_entries() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("create .claude");
    std::fs::write(
        repo.path().join(".mcp.json"),
        "{\n  \"mcpServers\": {\n    \"other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write mcp file");
    std::fs::write(
        repo.path().join(".claude.json"),
        "{\n  \"mcpServers\": {\n    \"orbit\": {\"command\": \"orbit\"},\n    \"legacy-other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write legacy mcp file");
    std::fs::write(
        repo.path().join(".claude").join("settings.json"),
        "{\n  \"permissions\": {\n    \"allow\": [\"OtherTool\"]\n  },\n  \"theme\": \"light\"\n}\n",
    )
    .expect("write settings");

    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let providers = run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init claude");
    assert_eq!(providers, vec![McpProvider::Claude]);

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".mcp.json")).expect("read mcp"),
    )
    .expect("parse mcp");
    assert!(mcp["mcpServers"]["orbit"].is_object());
    assert!(mcp["mcpServers"]["other"].is_object());
    let args = mcp["mcpServers"]["orbit"]["args"]
        .as_array()
        .expect("args array");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_str(), Some("mcp"));
    assert_eq!(args[1].as_str(), Some("serve"));

    let legacy_mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".claude.json")).expect("read legacy mcp"),
    )
    .expect("parse legacy mcp");
    assert!(legacy_mcp["mcpServers"]["orbit"].is_null());
    assert!(legacy_mcp["mcpServers"]["legacy-other"].is_object());

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".claude").join("settings.json"))
            .expect("read settings"),
    )
    .expect("parse settings");
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("allow array");
    assert!(allow.iter().any(|item| item == "OtherTool"));
    assert!(
        allow
            .iter()
            .any(|item| item == &claude_permission_name("orbit.task.show"))
    );
    // The AC names the exact post-fix shape literally; pin it here so a
    // regression in `claude_permission_name` cannot pass the test above.
    assert!(
        allow
            .iter()
            .any(|item| item == "mcp__orbit__orbit_task_show"),
        "Claude allowlist must contain literal `mcp__orbit__orbit_task_show` \
         (server-id-derived name for the CLI-registered `orbit` MCP server)",
    );
    assert!(
        !allow
            .iter()
            .any(|item| item.as_str().is_some_and(|s| s.starts_with("mcp__plugin_"))),
        "CLI init must not emit Claude Code plugin-scoped permission names; \
         that shape is synthesized by Claude itself for plugin installs",
    );
    assert_eq!(settings["theme"], "light");

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove claude");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".mcp.json")).expect("read mcp"),
    )
    .expect("parse mcp");
    assert!(mcp["mcpServers"]["orbit"].is_null());
    assert!(mcp["mcpServers"]["other"].is_object());
}

#[test]
fn claude_home_scope_uses_documented_user_file_and_cleans_legacy_file() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(home.path().join(".claude")).expect("create claude home");
    std::fs::write(
        home.path().join(".claude").join(".mcp.json"),
        "{\n  \"mcpServers\": {\n    \"orbit\": {\"command\": \"orbit\"},\n    \"legacy-other\": {\"command\": \"demo\"}\n  }\n}\n",
    )
    .expect("write legacy home mcp file");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init claude home scope");

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json")).expect("read user mcp"),
    )
    .expect("parse user mcp");
    assert!(mcp["mcpServers"]["orbit"].is_object());

    let legacy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude").join(".mcp.json"))
            .expect("read legacy user mcp"),
    )
    .expect("parse legacy user mcp");
    assert!(legacy["mcpServers"]["orbit"].is_null());
    assert!(legacy["mcpServers"]["legacy-other"].is_object());

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("remove claude home scope");

    assert!(!home.path().join(".claude.json").exists());
    let legacy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude").join(".mcp.json"))
            .expect("read legacy user mcp after remove"),
    )
    .expect("parse legacy user mcp after remove");
    assert!(legacy["mcpServers"]["orbit"].is_null());
    assert!(legacy["mcpServers"]["legacy-other"].is_object());
}

#[test]
fn claude_remove_deletes_empty_directory_init_created() {
    // ORB-12115: `init` calls `write_json_object`, which `create_dir_all`s
    // `.claude/` when it does not already exist. `remove` deleting only the
    // settings file it wrote left that directory behind, orphaned and empty.
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    assert!(!repo.path().join(".claude").exists(), "precondition");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init claude");
    assert!(
        repo.path().join(".claude").is_dir(),
        "init must create .claude/ when it is absent"
    );

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove claude");

    assert!(
        !repo.path().join(".claude").exists(),
        "remove must not leave behind the now-empty directory it created"
    );
}

#[test]
fn claude_remove_preserves_directory_with_unrelated_content() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude").join("commands"))
        .expect("create unrelated .claude subdirectory");
    std::fs::write(
        repo.path().join(".claude").join("commands").join("foo.md"),
        "unrelated command",
    )
    .expect("write unrelated file");

    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("init claude");

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove claude");

    assert!(
        repo.path()
            .join(".claude")
            .join("commands")
            .join("foo.md")
            .exists(),
        "remove must not touch directory content it did not create"
    );
}

#[test]
fn claude_operator_init_writes_single_operator_flag_and_refresh_is_idempotent() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("create .claude");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    let init = || {
        run_action(
            McpAction::Init(OPERATOR_LAUNCH),
            repo.path(),
            &orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
            Some(home.path().to_path_buf()),
            ScopeArg::Workspace,
        )
        .expect("operator init claude")
    };

    let assert_single_operator_entry = || {
        let mcp: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo.path().join(".mcp.json")).expect("read mcp"),
        )
        .expect("parse mcp");
        let args = mcp["mcpServers"]["orbit"]["args"]
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
    };

    init();
    assert_single_operator_entry();

    // Re-running the operator-authorized init (as `orbit workspace init
    // --force --mcp` does) must replace the entry with a single `--operator`
    // argument, not duplicate it.
    init();
    assert_single_operator_entry();
}

#[test]
fn claude_remove_strips_legacy_plugin_prefixed_entries() {
    // Pre-ORB-00286 the CLI wrote `mcp__plugin_orbit_orbit__*` entries
    // into Claude settings. After the fix `init` no longer emits them,
    // but existing user settings still carry them. `remove --claude`
    // must strip the legacy entries so an upgrade leaves a clean file,
    // while preserving unrelated `permissions.allow` entries.
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    std::fs::create_dir_all(repo.path().join(".claude")).expect("create .claude");
    std::fs::write(
        repo.path().join(".claude.json"),
        "{\n  \"mcpServers\": {\n    \"orbit\": {\"command\": \"orbit\", \"args\": [\"mcp\", \"serve\"]}\n  }\n}\n",
    )
    .expect("write mcp file");
    std::fs::write(
        repo.path().join(".claude").join("settings.json"),
        "{\n  \"permissions\": {\n    \"allow\": [\n      \"OtherTool\",\n      \"mcp__plugin_orbit_orbit__orbit_task_show\",\n      \"mcp__plugin_orbit_orbit__orbit_search\"\n    ]\n  }\n}\n",
    )
    .expect("write settings");

    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");

    run_action(
        McpAction::Remove,
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Workspace,
    )
    .expect("remove claude");

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(repo.path().join(".claude").join("settings.json"))
            .expect("read settings"),
    )
    .expect("parse settings");
    let allow = settings["permissions"]["allow"]
        .as_array()
        .expect("allow array");
    assert!(
        allow.iter().any(|item| item == "OtherTool"),
        "unrelated permission entries must survive remove",
    );
    assert!(
        !allow
            .iter()
            .any(|item| item.as_str().is_some_and(|s| s.starts_with("mcp__plugin_"))),
        "legacy plugin-prefixed entries must be stripped by remove",
    );
}
