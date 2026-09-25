//! The plugin callback boundary: allowlists, sessions, and ceilings.

use orbit_common::OrbitError;
use orbit_tools::plugin::PluginCallbackSession;
use orbit_types::plugin::{InstalledPlugin, PluginProvenance};
use orbit_types::telemetry::AuditEventStatus;
use std::path::Path;

use serde_json::json;

use super::super::ORBIT_PLUGIN_ENV;
use super::super::execute::ToolEntryPoint;
use crate::OrbitRuntime;
use crate::adapter::command::tests::support::{env_guard, fresh_runtime};
use crate::runtime::plugin::paths::plugin_install_path;

fn record_callback_plugin(runtime: &OrbitRuntime, orbit_tools: &[&str]) {
    let root = plugin_install_path(&runtime.global_root(), "callback", "1.0.0");
    record_callback_plugin_tree(&root, orbit_tools);
    runtime
        .stores()
        .plugins()
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
    let installed = runtime
        .stores()
        .plugins()
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

/// Turn the deprecation on for this host, so the retired environment token and
/// process ancestry identify a callback for one more release [ORB-12841].
fn enable_legacy_callback_identity(runtime: &OrbitRuntime) {
    let path = runtime.global_root().join("config.toml");
    let mut document = std::fs::read_to_string(&path).unwrap_or_default();
    document.push_str("\n[plugin]\nlegacy_callback_identity = true\n");
    std::fs::write(&path, document).expect("write the host config");
}

/// What the recorded manifest asks for under `permissions.orbit_tools`.
fn recorded_orbit_tools_request(runtime: &OrbitRuntime) -> Vec<String> {
    let installed = runtime
        .stores()
        .plugins()
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
#[test]
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

/// A backend already running cannot widen its own allowlist by relocating its
/// row: the callback gate reads `permissions.orbit_tools` out of the tree the
/// row names, so the path is held to the install root on every call, not only
/// at load [ORB-12785].
#[test]
fn plugin_callback_allowlist_refuses_a_row_repointed_outside_the_install_root() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    set_plugin_callback_env("callback", None);
    dispatch_cli(&runtime, "orbit.task.list").expect("the recorded install admits its own tool");

    // The attacker's tree, under a directory `orbit_tools` lets the backend
    // write, with a manifest that asks for everything.
    let global_root = runtime.global_root();
    let evil = global_root.join("state/logs/evil");
    record_callback_plugin_tree(&evil, &["orbit.task.list", "orbit.search"]);
    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("recorded");
    installed.install_path = evil.to_string_lossy().into_owned();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("the attacker's row write succeeds; the gate is what refuses it");

    for tool in ["orbit.search", "orbit.task.list"] {
        let error = dispatch_cli(&runtime, tool)
            .expect_err("no allowlist is read out of a tree outside the install root");
        let message = error.to_string();
        assert!(
            matches!(error, OrbitError::PolicyDenied(_))
                && message.contains(&installed.install_path)
                && message.contains(&global_root.join("plugins/callback").display().to_string()),
            "{tool}: {message}"
        );
    }
}

#[test]
fn plugin_callback_allowlist_is_idle_without_orbit_plugin() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::set_var("ORBIT_ALLOWED_TOOLS", "orbit.task.list");
    }
    dispatch_cli(&runtime, "orbit.search")
        .expect("ordinary CLI callers are not gated by a plugin allowlist");
}

/// Clearing `ORBIT_PLUGIN` in a live backend process does not drop the
/// allowlist: ancestry still names the plugin.
#[test]
fn plugin_callback_allowlist_holds_after_orbit_plugin_is_cleared() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
        std::env::remove_var("ORBIT_ALLOWED_TOOLS");
    }

    dispatch_cli(&runtime, "orbit.task.list")
        .expect("a recorded callback still runs after ORBIT_PLUGIN is cleared");
    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.search")
            .expect_err("clearing ORBIT_PLUGIN must not admit an unrecorded tool"),
        "orbit.search",
    );
}

#[test]
fn plugin_callback_allowlist_applies_on_mcp_entry_point() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    dispatch_entry(&runtime, "orbit.task.list", ToolEntryPoint::Mcp)
        .expect("a recorded callback still runs over MCP");
    assert_plugin_allowlist_denied(
        &dispatch_entry(&runtime, "orbit.search", ToolEntryPoint::Mcp)
            .expect_err("MCP must apply the same plugin allowlist as CLI"),
        "orbit.search",
    );
}

#[test]
fn plugin_callback_refusal_is_audited_with_plugin_identity() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session = bind_live_callback_session(&runtime);
    let _credential = present_callback_descriptor(&session);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.search").expect_err("unrecorded tool is refused"),
        "orbit.search",
    );

    let events = runtime
        .list_audit_events(None, Some("orbit.search".to_string()), None, None, 16)
        .expect("list audit events");
    let row = events
        .iter()
        .find(|event| event.status == AuditEventStatus::Denied)
        .expect("denied callback row");
    let plugin = row.plugin.as_ref().expect("plugin identity on the refusal");
    assert_eq!(plugin.name, "callback");
    assert_eq!(plugin.version, "1.0.0");
}

/// A backend descendant that changed process group and dropped the credential
/// is not an ordinary caller. The plugin sandbox is what it cannot shed: the
/// host-owned session directory stays unreadable to it, and an unreadable
/// session directory with no credential is a refusal on both entry points
/// [ORB-12798].
#[cfg(unix)]
#[test]
fn plugin_callback_refuses_an_unidentified_confined_child() {
    use std::os::unix::fs::PermissionsExt;

    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }

    let sessions = runtime.global_root().join("state/plugin-callbacks");
    std::fs::create_dir_all(&sessions).expect("create the session directory");
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000))
        .expect("make the session directory unreadable");
    if std::fs::read_dir(&sessions).is_ok() {
        // Root ignores directory permissions; the kernel-enforced case is the
        // CLI regression through the real plugin sandbox.
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700))
            .expect("restore");
        return;
    }

    let refusals: Vec<OrbitError> = [ToolEntryPoint::Cli, ToolEntryPoint::Mcp]
        .into_iter()
        .map(|entry_point| {
            dispatch_entry(&runtime, "orbit.task.list", entry_point)
                .expect_err("an unidentified plugin child is refused")
        })
        .collect();
    std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o700)).expect("restore");

    for error in refusals {
        assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
        assert!(
            error
                .to_string()
                .contains("without the host-issued callback session"),
            "{error}"
        );
    }
}

/// A live token identifies the process the host bound it to. Presenting one
/// that belongs to another process — read out of the session directory, or
/// kept across a `setsid` — is a mismatch, never that plugin's allowlist.
#[test]
fn plugin_callback_refuses_a_token_bound_to_another_process() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    enable_legacy_callback_identity(&runtime);
    let installed = runtime
        .stores()
        .plugins()
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
        &["orbit.task.list".to_string()],
    )
    .expect("mint callback session");
    // pid 1 is live and is never this process, its parent, or its group.
    session.bind_pid(1).expect("bind another process");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::set_var(
            orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV,
            session.token(),
        );
    }

    let error = dispatch_cli(&runtime, "orbit.task.list")
        .expect_err("a token bound to another process is not this caller's credential");
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
    }
    assert!(matches!(error, OrbitError::PolicyDenied(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("not held by the calling process"),
        "{error}"
    );
}

/// Clear every restriction the child controls: the namespace, the
/// informational allowlist, the callback token, and the activity envelope
/// that would otherwise bound `ToolContext.allowed_tools`. What remains is
/// the host-owned session, which is the whole point [ORB-12801].
fn shed_child_restrictions() {
    // SAFETY: callers hold `env_guard()` while changing process environment.
    unsafe {
        std::env::remove_var(ORBIT_PLUGIN_ENV);
        std::env::remove_var(orbit_tools::plugin::ORBIT_PLUGIN_CALLBACK_ENV);
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
#[test]
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

/// Two live sessions of the same plugin, minted for callers with different
/// allowlists. Each token carries its own ceiling; neither borrows the
/// other's.
#[test]
fn separate_callback_sessions_keep_their_own_ceilings() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list", "orbit.search"]);
    let lister =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let searcher = bind_live_callback_session_with_ceiling(&runtime, &["orbit.search".to_string()]);
    shed_child_restrictions();

    let as_session = |session: &PluginCallbackSession, tool: &str| {
        let _credential = present_callback_descriptor(session);
        dispatch_cli(&runtime, tool)
    };

    as_session(&lister, "orbit.task.list").expect("the lister's own tool");
    assert_ceiling_denied(
        &as_session(&lister, "orbit.search")
            .expect_err("the lister must not reach the searcher's tool"),
        "orbit.search",
    );
    as_session(&searcher, "orbit.search").expect("the searcher's own tool");
    assert_ceiling_denied(
        &as_session(&searcher, "orbit.task.list")
            .expect_err("the searcher must not reach the lister's tool"),
        "orbit.task.list",
    );
}

/// A backend can write its own install tree, so it can widen what the row's
/// manifest requests while it is still running. The recorded list is re-read
/// on every callback — and intersected with a ceiling that was fixed when the
/// host minted the session, so the widened request buys nothing.
#[test]
fn a_live_callback_session_does_not_widen_when_the_recorded_manifest_does() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();
    dispatch_cli(&runtime, "orbit.task.list").expect("the recorded install admits its own tool");

    // Rewrite the manifest in place, inside the install root the gate holds
    // the row to, so nothing but the requested allowlist changes.
    record_callback_plugin_tree(
        &plugin_install_path(&runtime.global_root(), "callback", "1.0.0"),
        &["orbit.task.list", "orbit.search"],
    );

    assert_ceiling_denied(
        &dispatch_cli(&runtime, "orbit.search")
            .expect_err("widening the recorded request must not widen a live session"),
        "orbit.search",
    );
    dispatch_cli(&runtime, "orbit.task.list")
        .expect("the session's own tool is unaffected by the rewrite");
}

/// The other direction, which the ceiling must not freeze: revocation. The
/// recorded grant is read on every callback, so withdrawing `orbit_tools`
/// stops a session that is already live at its next call — there is no
/// session to kill and no cache to invalidate.
#[test]
fn revoking_the_grant_stops_a_live_callback_session() {
    let _g = env_guard();
    let runtime = fresh_runtime();
    record_callback_plugin(&runtime, &["orbit.task.list"]);
    let session =
        bind_live_callback_session_with_ceiling(&runtime, &["orbit.task.list".to_string()]);
    let _credential = present_callback_descriptor(&session);
    shed_child_restrictions();
    dispatch_cli(&runtime, "orbit.task.list").expect("the granted callback runs");

    let mut installed = runtime
        .stores()
        .plugins()
        .get_plugin("callback")
        .expect("read plugin")
        .expect("recorded");
    installed.grants = Vec::new();
    runtime
        .stores()
        .plugins()
        .upsert_plugin(&installed)
        .expect("revoke orbit_tools");

    assert_plugin_allowlist_denied(
        &dispatch_cli(&runtime, "orbit.task.list")
            .expect_err("a revoked grant refuses the next callback of a live session"),
        "orbit.task.list",
    );
}
