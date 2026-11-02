//! [ORB-21263] `orbit mcp serve --role`: a role serves exactly its toolsets
//! in bound, federated and remote sessions, `--toolset` adds to it, a call
//! outside it is refused naming the role and the toolset, and a role never
//! grants operator authority.

use serde_json::json;

use super::{McpWorkspace, tool_names};

/// The reader role's advertised surface: core discovery plus task, run,
/// codebase and search reads.
const READER_TOOLS: [&str; 13] = [
    "orbit_codebase_analytics",
    "orbit_codebase_related",
    "orbit_codebase_show",
    "orbit_search",
    "orbit_task_artifact_get",
    "orbit_task_block_triage",
    "orbit_task_eligible",
    "orbit_task_list",
    "orbit_task_show",
    "orbit_workflow_run_list",
    "orbit_workflow_run_observe",
    "orbit_workflow_run_show",
    "orbit_workspace_list",
];

fn sorted(mut names: Vec<&str>) -> Vec<&str> {
    names.retain(|name| !name.starts_with("orbit_ui_"));
    names.sort_unstable();
    names
}

/// Write the host's `machine.mcp.role`.
fn set_host_role(workspace: &McpWorkspace, role: &str) {
    let config_path = workspace.home.join(".orbit/config.toml");
    let mut config: toml_edit::DocumentMut = std::fs::read_to_string(&config_path)
        .unwrap_or_default()
        .parse()
        .expect("fixture config TOML");
    config["machine"]["mcp"]["role"] = toml_edit::value(role);
    std::fs::write(&config_path, config.to_string()).expect("write host role");
}

/// An `ssh` on the fixture PATH that records the remote command and runs it
/// locally, with `orbit` resolving to the built binary, so `--mode remote`
/// reaches a real destination server over stdio.
#[cfg(unix)]
fn plant_loopback_ssh(workspace: &McpWorkspace) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = McpWorkspace::stub_bin_dir(&workspace.home);
    std::fs::create_dir_all(&bin).expect("create stub directory");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_orbit"), bin.join("orbit"))
        .expect("link orbit into the stub directory");
    let log = workspace.home.join("ssh-remote-commands.log");
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nfor last; do :; done\nprintf '%s\\n' \"$last\" >> '{}'\nexec sh -c \"$last\"\n",
            log.display()
        ),
    )
    .expect("write loopback ssh");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
        .expect("mark loopback ssh executable");
    log
}

#[test]
fn a_role_serves_only_its_toolsets_in_every_mode() {
    let workspace = McpWorkspace::init();
    for (case, args) in [
        ("bound", &["--operator", "--role", "reader"][..]),
        (
            "federated",
            &["--operator", "--mode", "federated", "--role", "reader"][..],
        ),
    ] {
        let mut client = workspace.serve_with_args(args);
        let listed = client.tools_list();
        assert_eq!(
            sorted(tool_names(&listed)),
            READER_TOOLS.to_vec(),
            "{case}: the reader role must advertise exactly its toolsets"
        );

        // A tool outside the role is refused, naming the role, its toolset
        // and the roles that include it.
        let refused =
            client.assert_tool_error("orbit_task_update", json!({}), "toolset_not_enabled");
        assert_eq!(refused["toolset"], "tasks-write", "{case}: {refused}");
        assert_eq!(refused["role"], "reader", "{case}: {refused}");
        assert_eq!(
            refused["enable"]["roles"],
            json!(["operator", "resident", "agent"]),
            "{case}: {refused}"
        );
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|message| message.contains("'reader' MCP role")),
            "{case}: {refused}"
        );
    }

    // `--toolset` adds to the role rather than replacing it.
    let mut widened =
        workspace.serve_with_args(&["--operator", "--role", "reader", "--toolset", "leases"]);
    let listed = widened.tools_list();
    let names = tool_names(&listed);
    assert!(names.contains(&"orbit_lease_list"), "{names:?}");
    assert!(names.contains(&"orbit_task_show"), "{names:?}");
    assert!(!names.contains(&"orbit_task_update"), "{names:?}");

    // `machine.mcp.role` is the host default; `--role` replaces it.
    set_host_role(&workspace, "manager");
    let mut configured = workspace.serve_with_args(&["--operator"]);
    let listed = configured.tools_list();
    let names = tool_names(&listed);
    assert!(names.contains(&"orbit_notice_list"), "{names:?}");
    assert!(names.contains(&"orbit_decision_list"), "{names:?}");
    assert!(!names.contains(&"orbit_task_update"), "{names:?}");
    let mut flagged = workspace.serve_with_args(&["--operator", "--role", "reader"]);
    let listed = flagged.tools_list();
    assert_eq!(sorted(tool_names(&listed)), READER_TOOLS.to_vec());
}

/// `--mode remote` asks the destination for the same role and extra
/// toolsets, and the destination applies them.
#[cfg(unix)]
#[test]
fn remote_mode_forwards_the_role_to_the_destination() {
    let workspace = McpWorkspace::init();
    let log = plant_loopback_ssh(&workspace);
    let mut client = workspace.serve_with_args(&[
        "--operator",
        "--mode",
        "remote",
        "loopback-host",
        "--role",
        "reader",
        "--toolset",
        "notice",
    ]);
    let listed = client.tools_list();
    let names = tool_names(&listed);
    for tool in READER_TOOLS {
        assert!(
            names.contains(&tool),
            "remote reader lost {tool}: {names:?}"
        );
    }
    assert!(names.contains(&"orbit_notice_list"), "{names:?}");
    assert!(!names.contains(&"orbit_task_update"), "{names:?}");
    let refused = client.assert_tool_error("orbit_task_add", json!({}), "toolset_not_enabled");
    assert_eq!(refused["role"], "reader", "{refused}");
    drop(client);

    let commands = std::fs::read_to_string(&log).expect("read the remote command log");
    assert!(
        commands.contains("--operator") && commands.contains("--role reader --toolset 'notice'"),
        "the destination must be asked for the role and the extra toolset: {commands}"
    );
}

/// A role limits the surface; it never grants authority. The operator role
/// still hides and refuses operator-only tools for an agent session.
#[test]
fn a_role_never_grants_operator_capability() {
    let workspace = McpWorkspace::init();
    for role in ["operator", "reader"] {
        let mut agent = workspace.serve_with_args(&["--role", role]);
        let listed = agent.tools_list();
        assert!(
            !tool_names(&listed).contains(&"orbit_workflow_run_list"),
            "{role}: an agent session must not be shown an operator-only tool"
        );
        agent.assert_tool_error("orbit_workflow_run_list", json!({}), "capability_denied");

        let mut operator = workspace.serve_with_args(&["--operator", "--role", role]);
        operator.call_tool_ok("orbit_workflow_run_list", json!({}));
    }
}

#[test]
fn an_unknown_role_is_refused_before_serving() {
    let workspace = McpWorkspace::init();
    let output = workspace.run(&["mcp", "serve", "--role", "admin"]);
    assert!(!output.status.success(), "{output:?}");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unknown MCP role 'admin'"), "{error}");
}
