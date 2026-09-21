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
