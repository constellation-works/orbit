//! The plugin callback boundary: allowlists, sessions, and ceilings.
//!
//! Callback credentials travel on an inherited descriptor, which only Unix
//! supports.
#![cfg(unix)]

use orbit_common::OrbitError;
use orbit_tools::plugin::PluginCallbackSession;
use orbit_types::plugin::{InstalledPlugin, PluginProvenance};
use std::path::Path;

use serde_json::json;

const ORBIT_PLUGIN_ENV: &str = "ORBIT_PLUGIN";
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;

use orbit_core::runtime::plugin::paths::plugin_install_path;

fn record_callback_plugin(runtime: &OrbitRuntime, orbit_tools: &[&str]) {
    let root = plugin_install_path(&runtime.global_root(), "callback", "1.0.0");
    record_callback_plugin_tree(&root, orbit_tools);
    crate::admission_audit::store_support::plugins(runtime)
        .upsert_plugin(&InstalledPlugin {
            name: "callback".to_string(),
            version: "1.0.0".to_string(),
            source: "fixture".to_string(),
            install_path: root.to_string_lossy().into_owned(),
            archive_digest: None,
            manifest_digest: "0".repeat(64),
            enabled: true,
            grants: vec!["orbit_tools".to_string()],
            first_party: false,
            certified_orbit_version: None,
            build: None,
            installed_at: String::new(),
            updated_at: String::new(),
        })
        .expect("record the install");
}

/// The plugin tree alone, so a test can write a second one somewhere the row
/// has no business pointing at.
fn record_callback_plugin_tree(root: &Path, orbit_tools: &[&str]) {
    let name = "callback";
    let version = "1.0.0";
    std::fs::create_dir_all(root.join("bin")).expect("create plugin bin");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ncat >/dev/null\necho '{\"ok\":true,\"output\":{}}'\n",
    )
    .expect("write backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod backend");
    }
    let requested = orbit_tools.join(", ");
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {name}\n  version: {version}\nspec:\n  permissions:\n    orbit_tools: [{requested}]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: hello\n      execution_kind: read_only\n      mcp_scope: workspace\n"
        ),
    )
    .expect("write manifest");
}

fn set_plugin_callback_env(plugin: &str, allowed_tools: Option<&str>) {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(ORBIT_PLUGIN_ENV, plugin);
        match allowed_tools {
            Some(value) => std::env::set_var("ORBIT_ALLOWED_TOOLS", value),
            None => std::env::remove_var("ORBIT_ALLOWED_TOOLS"),
        }
    }
}

/// A live session whose ceiling is everything the recorded install requests:
/// the spawning caller was unrestricted, which is the shape every test that
/// predates the ceiling assumes.
fn bind_live_callback_session(runtime: &OrbitRuntime) -> PluginCallbackSession {
    let requested = recorded_orbit_tools_request(runtime);
    bind_live_callback_session_with_ceiling(runtime, &requested)
}

/// The same session minted for a caller whose own allowlist was narrower than
/// the plugin's manifest request [ORB-12801].
fn bind_live_callback_session_with_ceiling(
    runtime: &OrbitRuntime,
    effective_tools: &[String],
) -> PluginCallbackSession {
    let installed = crate::admission_audit::store_support::plugins(runtime)
        .get_plugin("callback")
        .expect("read plugin")
        .expect("callback plugin is recorded");
    let mut session = PluginCallbackSession::mint(
        &runtime.global_root(),
        &PluginProvenance {
            name: installed.name,
            version: installed.version,
            manifest_digest: installed.manifest_digest,
            grants: installed.grants,
        },
        effective_tools,
    )
    .expect("mint callback session");
    session
        .bind_pid(std::process::id())
        .expect("bind this process as the plugin child");
    session
}

/// Hold a live session's record open on the descriptor a spawned backend
/// inherits, and name that descriptor to the resolver.
///
/// This is the credential in production: the host opens the record and maps it
/// onto file descriptor 3 in the child. Tests cannot dictate a process-wide
/// descriptor number, so they name the one they got — the same seam the
/// environment variable exists for. Dropping the returned file is what a
/// descendant that sheds the credential does.
fn present_callback_descriptor(session: &PluginCallbackSession) -> std::fs::File {
    use std::os::fd::AsRawFd;

    let file = std::fs::File::open(session.path()).expect("open the session record");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(
            orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV,
            file.as_raw_fd().to_string(),
        );
    }
    file
}

/// What the recorded manifest asks for under `permissions.orbit_tools`.
fn recorded_orbit_tools_request(runtime: &OrbitRuntime) -> Vec<String> {
    let installed = crate::admission_audit::store_support::plugins(runtime)
        .get_plugin("callback")
        .expect("read plugin")
        .expect("callback plugin is recorded");
    orbit_tools::plugin::load_plugin_dir(std::path::Path::new(&installed.install_path))
        .expect("load the recorded install")
        .manifest
        .spec
        .permissions
        .orbit_tools
}

fn dispatch_entry(
    runtime: &OrbitRuntime,
    tool: &str,
    entry_point: ToolEntryPoint,
) -> Result<serde_json::Value, OrbitError> {
    let input = match tool {
        "orbit.search" => json!({
            "query": "callback",
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL
        }),
        "orbit.task.list" => json!({ "limit": 10 }),
        other => panic!("dispatch fixture does not cover {other}"),
    };
    runtime
        .execute_tool_command_dispatch(tool, input, None, None, entry_point)
        .map(|outcome| outcome.value)
}

fn dispatch_cli(runtime: &OrbitRuntime, tool: &str) -> Result<serde_json::Value, OrbitError> {
    dispatch_entry(runtime, tool, ToolEntryPoint::Cli)
}

fn assert_plugin_allowlist_denied(error: &OrbitError, tool: &str) {
    let message = error.to_string();
    assert!(
        matches!(error, OrbitError::PolicyDenied(_)),
        "expected PolicyDenied, got {error}"
    );
    assert!(
        message.contains(tool) && message.contains("granted orbit_tools allowlist"),
        "{message}"
    );
}

/// A plugin callback that unsets or rewrites `ORBIT_ALLOWED_TOOLS` is still
/// bounded by the recorded install, not by the inherited variable.
fn plugin_callback_allowlist_ignores_forged_or_unset_env() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);

    let run = |allowed_tools: Option<&str>, tool: &str| {
        set_plugin_callback_env("callback", allowed_tools);
        dispatch_cli(&runtime, tool)
    };

    run(None, "orbit.task.list").expect("unset env still admits a recorded tool");
    run(Some("orbit.search,orbit.task.add"), "orbit.task.list")
        .expect("rewritten env still admits a recorded tool");

    assert_plugin_allowlist_denied(
        &run(None, "orbit.search").expect_err("unset env must not admit an unrecorded tool"),
        "orbit.search",
    );
    assert_plugin_allowlist_denied(
        &run(Some("orbit.search"), "orbit.search")
            .expect_err("rewritten env must not admit an unrecorded tool"),
        "orbit.search",
    );
}

/// Without the descriptor, neither a token nor live ancestry may authorize
/// dispatch. This covers the default refusal even with a readable session
/// directory, where the sandbox probe cannot identify the caller.
fn plugin_callback_refuses_retired_credentials() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    // Force an absent descriptor independently of the test runner's fd 3.
    // SAFETY: this isolated fixture holds `env_guard()` for all env changes.
    unsafe {
        std::env::set_var(
            orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV,
            "2147483647",
        );
        std::env::set_var("ORBIT_PLUGIN_CALLBACK", "retired-token");
    }
    for entry in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        let error = dispatch_entry(&runtime, "orbit.task.list", entry)
            .expect_err("an environment token alone must not authorize dispatch");
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    }
    // The environment the host actually issues must also refuse a detached
    // caller. An unbound session has no ancestry match, just as a descendant
    // that has left its original process tree has none.
    let marker_session = PluginCallbackSession::mint(
        &runtime.global_root(),
        &PluginProvenance {
            name: "callback".to_string(),
            version: "1.0.0".to_string(),
            manifest_digest: "0".repeat(64),
            grants: Vec::new(),
        },
        &["orbit.task.list".to_string()],
    )
    .expect("mint marker session");
    let mut issued_env = Vec::new();
    marker_session.stamp_env(&mut issued_env);
    let marker = issued_env
        .iter()
        .find(|(name, _)| name == "ORBIT_PLUGIN_CALLBACK")
        .expect("host must mark a backend for refusal without its descriptor");
    // SAFETY: the fixture still holds `env_guard()`.
    unsafe {
        std::env::set_var(&marker.0, &marker.1);
    }
    for entry in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        let error = dispatch_entry(&runtime, "orbit.task.list", entry)
            .expect_err("the host's marker must refuse callbacks without a descriptor or ancestry");
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    }
    // SAFETY: the fixture still holds `env_guard()`.
    unsafe {
        std::env::remove_var("ORBIT_PLUGIN_CALLBACK");
    }
    let _session = bind_live_callback_session(&runtime);
    for entry in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        let error = dispatch_entry(&runtime, "orbit.task.list", entry)
            .expect_err("a backend that shed its descriptor must not become an ordinary caller");
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
        assert!(error.to_string().contains("callback"), "{error}");
    }
}

/// Clear every restriction the child controls: the namespace, the
/// informational allowlist, the callback token, and the activity envelope
/// that would otherwise bound `ToolContext.allowed_tools`. What remains is
/// the host-owned session, which is the whole point [ORB-12801].
fn shed_child_restrictions() {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var("ORBIT_PLUGIN_CALLBACK");
        std::env::remove_var("ORBIT_ALLOWED_TOOLS");
        std::env::remove_var("ORBIT_ACTIVITY_TOOLS");
        std::env::remove_var("ORBIT_TASK_ACTOR_KIND");
    }
}

fn assert_ceiling_denied(error: &OrbitError, tool: &str) {
    assert_plugin_allowlist_denied(error, tool);
    assert!(
        error.to_string().contains("never widens"),
        "the refusal must name the session ceiling, not only the manifest: {error}"
    );
}

/// The manifest requests two tools and the host granted `orbit_tools`, but
/// the caller that spawned this backend could reach only one of them. The
/// second is refused on both entry points even though the child has shed
/// every restriction it carries in its own environment.
fn plugin_callback_cannot_exceed_the_spawning_callers_ceiling() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list", "orbit.search"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();

    for entry_point in [ToolEntryPoint::Cli, ToolEntryPoint::Mcp] {
        dispatch_entry(&runtime, "orbit.task.list", entry_point)
            .expect("the tool inside the caller's ceiling still runs");
        assert_ceiling_denied(
            &dispatch_entry(&runtime, "orbit.search", entry_point).expect_err(
                "a manifest-listed tool outside the caller's ceiling must not be dispatched",
            ),
            "orbit.search",
        );
    }
}

fn env_guard() -> orbit_common::test_env::ScopedEnv {
    orbit_common::test_env::unset([
        "ORBIT_AGENT_NAME",
        "ORBIT_AGENT_MODEL",
        "ORBIT_TASK_ID",
        "ORBIT_RUN_ID",
        "ORBIT_MANAGED_RUN_CONTEXT",
        "ORBIT_ACTIVITY_ID",
        "ORBIT_STEP_INDEX",
        ORBIT_PLUGIN_ENV,
        "ORBIT_PLUGIN_CALLBACK",
        orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_FD_ENV,
        "ORBIT_ALLOWED_TOOLS",
    ])
}
fn fresh_runtime() -> OrbitRuntime {
    let _ = orbit_core::adapter::command::take_tool_audit_recorded();
    OrbitRuntime::in_memory().expect("build in-memory runtime")
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    plugin_callback_allowlist_ignores_forged_or_unset_env_scenarios,
    plugin_callback_allowlist_ignores_forged_or_unset_env,
    plugin_callback_refuses_retired_credentials,
    plugin_callback_cannot_exceed_the_spawning_callers_ceiling
);
