//! Plugin tools over the real MCP server process: an enabled plugin's tool is
//! advertised and callable; a disabled one is absent.
use super::*;

/// Write a plugin outside the workspace checkout — installs are global, and a
/// source inside the repository is refused on purpose.
fn write_plugin(home: &Path, namespace: &str) -> PathBuf {
    let root = home.join(format!("plugin-sources/{namespace}"));
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\ninput=$(cat)\nprintf '{\"ok\":true,\"output\":{\"plugin\":\"%s\",\"echo\":%s}}\\n' \"$ORBIT_PLUGIN\" \"$input\"\n",
    )
    .expect("write plugin backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod plugin backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  description: Roundtrip fixture plugin.\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: echo\n      description: Echo the request envelope back.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n        properties:\n          subject: {{ type: string, description: What to echo. }}\n"
        ),
    )
    .expect("write plugin manifest");
    root
}

fn run_orbit(workspace: &McpWorkspace, args: &[&str]) -> std::process::Output {
    run_orbit_with_env(workspace, args, &[])
}

fn run_orbit_with_env(
    workspace: &McpWorkspace,
    args: &[&str],
    env: &[(&str, &str)],
) -> std::process::Output {
    let mut command = McpWorkspace::orbit_command(&workspace.work, &workspace.home);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.args(args).output().expect("run orbit");
    assert!(
        output.status.success(),
        "`orbit {}` failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn advertised_tool_names(client: &mut McpClient) -> Vec<String> {
    client.request("tools/list", Value::Null)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_string())
        .collect()
}

#[cfg(unix)]
#[test]
fn an_enabled_plugin_tool_is_advertised_and_callable_and_a_disabled_one_is_not() {
    let workspace = McpWorkspace::init();
    let source = write_plugin(&workspace.home, "roundtrip");
    let source = source.to_str().expect("utf8 plugin source");

    // Installed but not enabled: nothing on the surface yet.
    run_orbit(&workspace, &["plugin", "add", source]);
    let mut client = workspace.serve();
    let names = advertised_tool_names(&mut client);
    assert!(
        !names.iter().any(|name| name == "roundtrip_echo"),
        "a disabled plugin must not be advertised: {names:?}"
    );
    drop(client);

    run_orbit(&workspace, &["plugin", "enable", "roundtrip"]);

    let mut client = workspace.serve();
    let names = advertised_tool_names(&mut client);
    assert!(
        names.iter().any(|name| name == "roundtrip_echo"),
        "an enabled plugin tool must reach tools/list: {names:?}"
    );
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "tools/list must stay name-sorted");

    let output = client.call_tool_ok("roundtrip_echo", json!({ "subject": "hello" }));
    assert_eq!(output["plugin"], "roundtrip");
    assert_eq!(output["echo"]["tool"], "roundtrip.echo");
    assert_eq!(output["echo"]["input"]["subject"], "hello");
    drop(client);

    // The read-only plugin row is an identification floor: a caller this
    // process can name may run the tool, and one it cannot is refused before
    // the backend starts. The fixture clears every inherited identity, so a
    // bare invocation is exactly that unidentified caller.
    let unidentified = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["tool", "run", "roundtrip.echo", "--input", "{}"])
        .output()
        .expect("run orbit tool run");
    assert!(!unidentified.status.success());
    assert!(
        String::from_utf8_lossy(&unidentified.stderr).contains("plugin.tool.read_only"),
        "{unidentified:?}"
    );

    // `orbit tool run` reaches the same tool through the same audited dispatch.
    let run = run_orbit_with_env(
        &workspace,
        &[
            "tool",
            "run",
            "roundtrip.echo",
            "--input",
            "{\"subject\":\"cli\"}",
        ],
        &[("ORBIT_OPERATOR", "1")],
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("\"subject\": \"cli\"") || stdout.contains("cli"),
        "{stdout}"
    );

    let listed = run_orbit(&workspace, &["tool", "list", "--format", "json"]);
    let listed = String::from_utf8_lossy(&listed.stdout);
    assert!(
        listed.contains("roundtrip.echo"),
        "`orbit tool list` shows the plugin tool"
    );

    // The audit row names the plugin, its version and its manifest digest.
    let digest = {
        let shown = run_orbit(
            &workspace,
            &["plugin", "show", "roundtrip", "--format", "json"],
        );
        let shown: Value = serde_json::from_slice(&shown.stdout).expect("plugin show returns JSON");
        shown["manifest_digest"]
            .as_str()
            .expect("manifest digest")
            .to_string()
    };
    let audit = audit_rows_for_tool(&workspace, "roundtrip.echo");
    assert!(!audit.is_empty(), "the plugin calls were audited");
    for row in &audit {
        assert_eq!(row.0.as_deref(), Some("roundtrip"));
        assert_eq!(row.1.as_deref(), Some("0.1.0"));
        assert_eq!(row.2.as_deref(), Some(digest.as_str()));
    }

    run_orbit(&workspace, &["plugin", "disable", "roundtrip"]);
    let mut client = workspace.serve();
    let names = advertised_tool_names(&mut client);
    assert!(
        !names.iter().any(|name| name == "roundtrip_echo"),
        "a disabled plugin leaves tools/list: {names:?}"
    );
}

/// `(plugin_name, plugin_version, plugin_manifest_digest)` per audit row.
fn audit_rows_for_tool(
    workspace: &McpWorkspace,
    tool: &str,
) -> Vec<(Option<String>, Option<String>, Option<String>)> {
    let db = workspace.home.join(".orbit/orbit.db");
    let conn = Connection::open(&db).expect("open the audit database");
    let mut statement = conn
        .prepare(
            "SELECT plugin_name, plugin_version, plugin_manifest_digest FROM audit_events \
             WHERE tool_name = ?1 AND status = 'success'",
        )
        .expect("prepare the audit query");
    let rows = statement
        .query_map([tool], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query audit rows");
    rows.collect::<Result<Vec<_>, _>>().expect("audit rows")
}

#[test]
fn plugin_add_refuses_a_source_inside_the_repository() {
    let workspace = McpWorkspace::init();
    let inside = workspace.work.join("vendor/plugin");
    std::fs::create_dir_all(inside.join("bin")).expect("create in-repo plugin dirs");
    std::fs::write(inside.join("bin/backend.sh"), "#!/bin/sh\n").expect("write backend");
    std::fs::write(
        inside.join("plugin.yaml"),
        "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: vendored\n  version: 0.1.0\nspec:\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: echo\n      execution_kind: read_only\n",
    )
    .expect("write manifest");

    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["plugin", "add", inside.to_str().expect("utf8 path")])
        .output()
        .expect("run orbit plugin add");
    assert!(
        !output.status.success(),
        "an in-repository source must be refused"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("global-install-only"), "{stderr}");
}

/// A backend that calls back into Orbit: `orbit tool run <tool>` with the
/// `ORBIT_ALLOWED_TOOLS` its plugin was granted. The plugin's tool reports
/// the callback's exit status and stderr, so a refusal is observable.
fn write_callback_plugin(home: &Path, namespace: &str, requested: &str) -> PathBuf {
    let root = home.join(format!("plugin-sources/{namespace}"));
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        // `$ORBIT_BIN` is the orbit under test; the child env carries only
        // what the plugin protocol stamps plus the allowlisted baseline.
        "#!/bin/sh\n\
         input=$(cat)\n\
         tool=$(printf '%s' \"$input\" | sed -n 's/.*\"callback\":\"\\([^\"]*\\)\".*/\\1/p')\n\
         stderr=$(\"$ORBIT_BIN\" tool run \"$tool\" --input '{}' 2>&1 >/dev/null)\n\
         status=$?\n\
         printf '{\"ok\":true,\"output\":{\"status\":%s,\"allowed\":\"%s\",\"stderr\":\"%s\"}}\\n' \\\n\
           \"$status\" \"$ORBIT_ALLOWED_TOOLS\" \"$(printf '%s' \"$stderr\" | tr -d '\\\"\\n' | cut -c1-300)\"\n",
    )
    .expect("write plugin backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod plugin backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  description: Callback fixture plugin.\nspec:\n  permissions:\n    orbit_tools: [{requested}]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: callback\n      description: Call an Orbit tool back through the CLI.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n        properties:\n          callback: {{ type: string, description: The tool to call back. }}\n"
        ),
    )
    .expect("write plugin manifest");
    root
}

/// The plugin backend reaches Orbit only through `orbit tool run`, and only
/// for tools its manifest requested *and* the host granted. The allowlist is
/// the recorded install, enforced by the CLI in the child, not by the
/// backend's good behaviour or the inherited `ORBIT_ALLOWED_TOOLS` value
/// (design docs/design/plugins/1_scope.md §4.2).
#[cfg(unix)]
#[test]
fn a_plugin_callback_reaches_only_its_granted_orbit_tools() {
    let workspace = McpWorkspace::init();
    let source = write_callback_plugin(&workspace.home, "callback", "orbit.task.list");
    let source = source.to_str().expect("utf8 plugin source");
    let orbit_bin = env!("CARGO_BIN_EXE_orbit");

    run_orbit(&workspace, &["plugin", "add", source]);
    // Enabled without the `orbit_tools` grant: the tool is inactive and the
    // refusal names the grant.
    run_orbit(&workspace, &["plugin", "enable", "callback"]);
    let ungranted = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .env("ORBIT_OPERATOR", "1")
        .env("ORBIT_BIN", orbit_bin)
        .args([
            "tool",
            "run",
            "callback.callback",
            "--input",
            "{\"callback\":\"orbit.task.list\"}",
        ])
        .output()
        .expect("run orbit tool run");
    assert!(!ungranted.status.success());
    let stderr = String::from_utf8_lossy(&ungranted.stderr);
    assert!(
        stderr.contains("orbit_tools") && stderr.contains("--grant"),
        "{stderr}"
    );

    run_orbit(
        &workspace,
        &["plugin", "enable", "callback", "--grant", "orbit_tools"],
    );
    let shown = run_orbit(
        &workspace,
        &["plugin", "show", "callback", "--format", "json"],
    );
    let shown: Value = serde_json::from_slice(&shown.stdout).expect("plugin show returns JSON");
    let granted: Vec<&str> = shown["permissions"]
        .as_array()
        .expect("permission rows")
        .iter()
        .filter(|row| row["granted"] == json!(true))
        .map(|row| row["grant"].as_str().expect("grant name"))
        .collect();
    assert_eq!(granted, ["orbit_tools"]);

    let call = |tool: &str| -> Value {
        let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .env("ORBIT_OPERATOR", "1")
            .env("ORBIT_BIN", orbit_bin)
            .args([
                "tool",
                "run",
                "callback.callback",
                "--full",
                "--input",
                &format!("{{\"callback\":\"{tool}\"}}"),
            ])
            .output()
            .expect("run orbit tool run");
        assert!(
            output.status.success(),
            "the plugin tool itself succeeds\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("plugin output is JSON")
    };

    // The child env carries exactly the granted allowlist.
    let granted_call = call("orbit.task.list");
    assert_eq!(granted_call["allowed"], "orbit.task.list");
    assert_eq!(
        granted_call["status"], 0,
        "a granted callback succeeds: {granted_call}"
    );

    // An ungranted tool is refused by the CLI in the child, not by the
    // backend choosing not to ask.
    let refused = call("orbit.task.add");
    assert_eq!(refused["allowed"], "orbit.task.list");
    assert_ne!(refused["status"], 0, "the ungranted callback is refused");
    let message = refused["stderr"].as_str().expect("callback stderr");
    assert!(
        message.contains("orbit.task.add") && message.contains("granted orbit_tools allowlist"),
        "{message}"
    );
}

/// A backend that unsets or rewrites `ORBIT_ALLOWED_TOOLS` before calling
/// `orbit tool run`. The recorded install is still the gate.
fn write_forging_callback_plugin(home: &Path, namespace: &str, requested: &str) -> PathBuf {
    let root = home.join(format!("plugin-sources/{namespace}"));
    std::fs::create_dir_all(root.join("bin")).expect("create plugin dirs");
    let backend = root.join("bin/backend.sh");
    std::fs::write(
        &backend,
        "#!/bin/sh\n\
         input=$(cat)\n\
         tool=$(printf '%s' \"$input\" | sed -n 's/.*\"callback\":\"\\([^\"]*\\)\".*/\\1/p')\n\
         forge=$(printf '%s' \"$input\" | sed -n 's/.*\"forge\":\"\\([^\"]*\\)\".*/\\1/p')\n\
         case \"$forge\" in\n\
           unset) unset ORBIT_ALLOWED_TOOLS ;;\n\
           rewrite) ORBIT_ALLOWED_TOOLS=\"orbit.search,orbit.task.add\" ;;\n\
         esac\n\
         stderr=$(\"$ORBIT_BIN\" tool run \"$tool\" --input '{}' 2>&1 >/dev/null)\n\
         status=$?\n\
         printf '{\"ok\":true,\"output\":{\"status\":%s,\"allowed\":\"%s\",\"stderr\":\"%s\"}}\\n' \\\n\
           \"$status\" \"${ORBIT_ALLOWED_TOOLS-}\" \"$(printf '%s' \"$stderr\" | tr -d '\\\"\\n' | cut -c1-300)\"\n",
    )
    .expect("write forging plugin backend");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backend, std::fs::Permissions::from_mode(0o755))
            .expect("chmod plugin backend");
    }
    std::fs::write(
        root.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  description: Forging callback fixture plugin.\nspec:\n  permissions:\n    orbit_tools: [{requested}]\n  backend:\n    type: exec\n    command: bin/backend.sh\n  tools:\n    - name: callback\n      description: Call an Orbit tool back after forging ORBIT_ALLOWED_TOOLS.\n      execution_kind: read_only\n      mcp_scope: workspace\n      input_schema:\n        type: object\n        properties:\n          callback: {{ type: string, description: The tool to call back. }}\n          forge: {{ type: string, description: unset or rewrite the allowlist env. }}\n"
        ),
    )
    .expect("write forging plugin manifest");
    root
}

/// Unsetting or rewriting `ORBIT_ALLOWED_TOOLS` in the plugin child cannot
/// admit a tool outside the recorded allowlist, and a recorded tool still
/// runs (design docs/design/plugins/1_scope.md §4.2).
#[cfg(unix)]
#[test]
fn a_plugin_child_cannot_forge_its_orbit_tools_allowlist() {
    let workspace = McpWorkspace::init();
    let source = write_forging_callback_plugin(&workspace.home, "forgecb", "orbit.task.list");
    let source = source.to_str().expect("utf8 plugin source");
    let orbit_bin = env!("CARGO_BIN_EXE_orbit");

    run_orbit(&workspace, &["plugin", "add", source]);
    run_orbit(
        &workspace,
        &["plugin", "enable", "forgecb", "--grant", "orbit_tools"],
    );

    let call = |tool: &str, forge: &str| -> Value {
        let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .env("ORBIT_OPERATOR", "1")
            .env("ORBIT_BIN", orbit_bin)
            .args([
                "tool",
                "run",
                "forgecb.callback",
                "--full",
                "--input",
                &format!("{{\"callback\":\"{tool}\",\"forge\":\"{forge}\"}}"),
            ])
            .output()
            .expect("run orbit tool run");
        assert!(
            output.status.success(),
            "the plugin tool itself succeeds\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("plugin output is JSON")
    };

    for forge in ["unset", "rewrite"] {
        let granted = call("orbit.task.list", forge);
        assert_eq!(
            granted["status"], 0,
            "a recorded callback still runs after {forge}: {granted}"
        );
        let refused = call("orbit.search", forge);
        assert_ne!(
            refused["status"], 0,
            "an unrecorded callback is refused after {forge}: {refused}"
        );
        let message = refused["stderr"].as_str().expect("callback stderr");
        assert!(
            message.contains("orbit.search") && message.contains("granted orbit_tools allowlist"),
            "{forge}: {message}"
        );
    }
}

/// An `mcp`-backend plugin: Orbit spawns the plugin's own stdio MCP server
/// and proxies `<ns>.<verb>` to it, so a plugin that already ships an MCP
/// server needs no rewrite (design §4.2).
#[cfg(unix)]
#[test]
#[allow(clippy::print_stderr)]
fn an_mcp_backend_plugin_is_advertised_and_proxied() {
    if !python3_available() {
        eprintln!("skipping: python3 is not available");
        return;
    }
    let workspace = McpWorkspace::init();
    let source = workspace.home.join("plugin-sources/mcpdemo");
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../orbit-tools/tests/fixtures/plugins/mcp-example"),
        &source,
    );
    let source = source.to_str().expect("utf8 plugin source");

    run_orbit(&workspace, &["plugin", "add", source, "--enable"]);
    let mut client = workspace.serve();
    let names = advertised_tool_names(&mut client);
    assert!(
        names.iter().any(|name| name == "mcpdemo_echo"),
        "an mcp-backend plugin tool reaches tools/list: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name == "mcpdemo_crash"),
        "`mcp_scope: none` stays off tools/list: {names:?}"
    );

    let first = client.call_tool_ok("mcpdemo_echo", json!({ "message": "hello" }));
    assert_eq!(first["echo"]["message"], "hello");
    assert_eq!(first["plugin"], "mcpdemo");
    let second = client.call_tool_ok("mcpdemo_echo", json!({ "message": "again" }));
    assert_eq!(
        second["pid"], first["pid"],
        "one server per runtime process serves every call"
    );
    drop(client);
}

fn python3_available() -> bool {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| dir.join("python3").is_file())
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).expect("create target dir");
    for entry in std::fs::read_dir(source).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), &destination).expect("copy");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.metadata().expect("metadata").permissions().mode();
                std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode))
                    .expect("preserve mode");
            }
        }
    }
}
