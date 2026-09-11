use std::path::PathBuf;

use tempfile::tempdir;

use orbit_common::fs::io::{FileLockOptions, acquire_exclusive_file_lock, atomic_write_text};

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
        home.path().join(".claude.json"),
        "{\n  \"userState\": \"preserve-me\"\n}\n",
    )
    .expect("write existing Claude state");
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
    assert_eq!(mcp["userState"], "preserve-me");
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

    let mcp: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude.json"))
            .expect("read preserved user mcp after remove"),
    )
    .expect("parse preserved user mcp after remove");
    assert_eq!(mcp["userState"], "preserve-me");
    assert!(mcp.get("mcpServers").is_none());
    let legacy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".claude").join(".mcp.json"))
            .expect("read legacy user mcp after remove"),
    )
    .expect("parse legacy user mcp after remove");
    assert!(legacy["mcpServers"]["orbit"].is_null());
    assert!(legacy["mcpServers"]["legacy-other"].is_object());
}

#[test]
fn claude_home_scope_waits_for_concurrent_state_update_before_reading() {
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let mcp_path = home.path().join(".claude.json");
    std::fs::write(&mcp_path, "{\n  \"userState\": \"before\"\n}\n")
        .expect("write initial Claude state");

    // Simulate Claude Code itself, which locks `<mcp_path>.lock` — the full
    // file name with `.lock` appended, not Orbit's usual dot-prefixed
    // sibling. Holding that literal path (independent of the production
    // helper) is what proves Orbit actually waits on Claude Code's own lock
    // rather than a differently-named file neither process contends on.
    let mut claude_lock_path = mcp_path.clone().into_os_string();
    claude_lock_path.push(".lock");
    let claude_lock_path = PathBuf::from(claude_lock_path);

    let (lock_ready_tx, lock_ready_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let held_path = claude_lock_path.clone();
    let holder = std::thread::spawn(move || {
        let _guard = acquire_exclusive_file_lock(
            &held_path,
            "test Claude Code writer",
            FileLockOptions::default(),
        )
        .expect("hold Claude lock");
        lock_ready_tx
            .send(())
            .expect("notify that Claude lock is held");
        release_rx.recv().expect("wait for test release");
    });
    lock_ready_rx.recv().expect("wait for Claude lock holder");

    let worker_repo = repo.path().to_path_buf();
    let worker_home = home.path().to_path_buf();
    let worker_orbit_root = orbit_root.clone();
    let worker = std::thread::spawn(move || {
        run_action(
            McpAction::Init(ServerLaunch::default()),
            &worker_repo,
            &worker_orbit_root,
            ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
            Some(worker_home),
            ScopeArg::Home,
        )
    });

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "Claude init must wait for the state-file lock before reading"
    );
    atomic_write_text(&mcp_path, "{\n  \"userState\": \"during\"\n}\n")
        .expect("write concurrent Claude state update");
    release_tx.send(()).expect("release Claude lock");
    holder.join().expect("join Claude lock holder");
    worker
        .join()
        .expect("join Claude init")
        .expect("Claude init after concurrent update");

    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).expect("read final Claude state"))
            .expect("parse final Claude state");
    assert_eq!(mcp["userState"], "during");
    assert!(mcp["mcpServers"]["orbit"].is_object());
}

#[test]
fn claude_home_scope_locks_claude_codes_exact_lock_file() {
    // ORB-12182: the home-scope read-modify-write must serialize against the
    // literal lock file Claude Code itself uses (`<mcp_path>.lock`), not a
    // dot-prefixed sibling of Orbit's own invention. Compute the expected
    // path independently of the production lock-naming helper so a rename of
    // that helper's convention fails this test instead of passing vacuously.
    let repo = tempdir().expect("repo tempdir");
    let home = tempdir().expect("home tempdir");
    let orbit_root = repo.path().join(".orbit");
    std::fs::create_dir_all(&orbit_root).expect("create orbit root");
    let mcp_path = home.path().join(".claude.json");

    run_action(
        McpAction::Init(ServerLaunch::default()),
        repo.path(),
        &orbit_root,
        ProviderSelectionMode::Explicit(vec![McpProvider::Claude]),
        Some(home.path().to_path_buf()),
        ScopeArg::Home,
    )
    .expect("init claude home scope");

    let mut expected_lock_path = mcp_path.clone().into_os_string();
    expected_lock_path.push(".lock");
    let expected_lock_path = PathBuf::from(expected_lock_path);
    assert!(
        expected_lock_path.is_file(),
        "expected Claude Code's lock file at {}",
        expected_lock_path.display()
    );

    let stray_lock_path = home.path().join("..claude.json.lock");
    assert!(
        !stray_lock_path.exists(),
        "must not create the generic dot-prefixed sibling lock file \
         Claude Code does not recognize: {}",
        stray_lock_path.display()
    );
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
