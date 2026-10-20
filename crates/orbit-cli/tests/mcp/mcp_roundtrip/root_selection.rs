//! Root isolation across the real stdio and TCP MCP entrypoints.

use super::*;

#[test]
fn workspace_discovery_redacts_legacy_registry_credentials() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::root_selection::workspace_discovery_redacts_legacy_registry_credentials",
    ) {
        return;
    }
    let workspace = McpWorkspace::init();
    let registry_path = workspace.home.join(".orbit/workspaces.json");
    let mut registry: Value =
        serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
    registry["workspaces"][0]["git_remote"] =
        json!("https://operator:fixture-secret@github.com/example/orbit.git");
    std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let before = std::fs::read(&registry_path).unwrap();
    let mut expected = registry["workspaces"][0].clone();
    expected["git_remote"] = json!("https://***@github.com/example/orbit.git");
    let local = workspace
        .serve()
        .call_tool_ok("orbit_workspace_list", json!({}));
    assert_eq!(local["workspaces"], json!([expected.clone()]));
    let federated = workspace
        .serve_with_args(&["--mode", "federated"])
        .call_tool_ok("orbit_workspace_list", json!({}));
    let row = federated["workspaces"][0].as_object().unwrap();
    for (key, value) in expected.as_object().unwrap() {
        assert_eq!(&row[key], value, "federated field {key}");
    }
    let show = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
            "workspace",
            "show",
            "--json",
        ]),
    );
    let show: Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show["workspace"]["git_remote"], expected["git_remote"]);
    let text = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args(["workspace", "show"]),
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains(expected["git_remote"].as_str().unwrap()));
    assert!(!text.contains("fixture-secret"));
    let list = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
            "workspace",
            "list",
            "--json",
        ]),
    );
    let list: Value = serde_json::from_slice(&list.stdout).unwrap();
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get("git_remote").is_none());
    assert_eq!(
        std::fs::read(&registry_path).unwrap(),
        before,
        "discovery must not rewrite legacy registrations"
    );
}

#[test]
fn workspace_discovery_excludes_removed_checkout_without_persisting() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::root_selection::workspace_discovery_excludes_removed_checkout_without_persisting",
    ) {
        return;
    }
    removed_checkout_discovery(false);
}

#[test]
fn federated_workspace_discovery_reports_removed_checkout_without_persisting() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::root_selection::federated_workspace_discovery_reports_removed_checkout_without_persisting",
    ) {
        return;
    }
    removed_checkout_discovery(true);
}

fn removed_checkout_discovery(federated: bool) {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve_with_args(if federated {
        &["--mode", "federated"]
    } else {
        &[]
    });
    let initial = client.call_tool_ok("orbit_workspace_list", json!({}));
    let id = initial["workspaces"][0]["id"].clone();
    assert!(id.is_string(), "{initial}");
    assert_eq!(initial["workspaces"][0]["status"], "active");
    let registry_path = workspace.home.join(".orbit/workspaces.json");
    let before = std::fs::read(&registry_path).unwrap();
    let moved = workspace.work.with_extension("removed");
    std::fs::rename(&workspace.work, &moved).unwrap();
    let listing = client.call_tool_ok("orbit_workspace_list", json!({}));
    if federated {
        let rows = listing["workspaces"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{listing}");
        assert_eq!(rows[0]["id"], id);
        assert_eq!(rows[0]["status"], "invalid");
        assert_eq!(rows[0]["checkout_health"], "invalid");
        assert_eq!(rows[0]["reachability"], "reachable");
    } else {
        assert_eq!(listing["workspaces"], json!([]));
    }
    assert_eq!(
        std::fs::read(&registry_path).unwrap(),
        before,
        "MCP discovery must validate checkout health without persisting it"
    );
    std::fs::rename(&moved, &workspace.work).unwrap();
    let restored = client.call_tool_ok("orbit_workspace_list", json!({}));
    assert_eq!(restored["workspaces"][0]["id"], id);
    assert_eq!(restored["workspaces"][0]["status"], "active");
}

fn discovered<C>(client: &mut McpClient<C>) -> Value {
    let initialized = client.initialize(McpClient::initialize_params("root-selection", None));
    assert!(initialized["result"].is_object(), "{initialized}");
    client.call_tool_ok("orbit_workspace_list", json!({}))
}

fn assert_selected(actual: &Value, expected: &Value) {
    // Federated discovery carries machine identity on each destination row.
    let machine = actual["machine_id"]
        .as_str()
        .or_else(|| actual["workspaces"][0]["machine_id"].as_str());
    assert_eq!(machine, expected["machine_id"].as_str(), "{actual}");
    let names = |value: &Value| {
        value["workspaces"]
            .as_array()
            .expect("workspace descriptors")
            .iter()
            .map(|workspace| workspace["name"].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(actual), names(expected), "{actual}");
}

fn stdio_discovery(command: &mut Command, args: &[&str]) -> Value {
    let child = command
        .args(["mcp", "serve"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map(ChildGuard::new)
        .expect("start root-isolated stdio server");
    discovered(&mut McpClient::new(child))
}

fn listener_discovery(command: &mut Command) -> Value {
    let mut child = BoundServer::spawn(
        command
            .args(["mcp", "listen", "127.0.0.1:0"])
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
        Announcement::Mcp,
    )
    .expect("start root-isolated listener");
    let addr = child
        .wait_for_address(RESPONSE_TIMEOUT)
        .expect("root-isolated listener bind announcement");
    let stream = TcpStream::connect(addr).expect("connect root-isolated listener");
    discovered(&mut McpClient::over_tcp(child, stream))
}

#[test]
fn mcp_transports_honor_explicit_data_roots_and_managed_registry_fallback() {
    // Both stores are disposable: a regression must never touch the host registry.
    let ambient = McpWorkspace::init_with_workspace_name("ambient");
    let selected = McpWorkspace::init_with_workspace_name("selected");
    let root = selected.home.join(".orbit");
    let expected = selected
        .serve()
        .call_tool_ok("orbit_workspace_list", json!({}));
    let mut ambient_client = ambient.serve();
    let ambient_identity = ambient_client.call_tool_ok("orbit_workspace_list", json!({}));
    assert_ne!(expected["machine_id"], ambient_identity["machine_id"]);
    assert_ne!(expected["workspaces"], json!([]));
    drop(ambient_client);

    for federated in [false, true] {
        let actual = stdio_discovery(
            McpWorkspace::orbit_command(&ambient.work, &ambient.home).env("ORBIT_ROOT", &root),
            if federated {
                &["--mode", "federated"]
            } else {
                &[]
            },
        );
        assert_selected(&actual, &expected);
    }
    assert_selected(
        &listener_discovery(
            McpWorkspace::orbit_command(&ambient.work, &ambient.home).env("ORBIT_ROOT", &root),
        ),
        &expected,
    );

    // The advertised --root flag must override a conflicting ORBIT_ROOT.
    let explicit = || {
        let mut command = McpWorkspace::orbit_command(&ambient.work, &ambient.home);
        command
            .arg("--root")
            .arg(&root)
            .env("ORBIT_ROOT", ambient.home.join(".orbit"));
        command
    };
    assert_selected(&stdio_discovery(&mut explicit(), &[]), &expected);
    assert_selected(&listener_discovery(&mut explicit()), &expected);

    // No explicit root: managed children still use the dispatching registry.
    let managed = || {
        let mut command = McpWorkspace::orbit_command(&ambient.work, &ambient.home);
        command
            .env("ORBIT_MANAGED_RUN_CONTEXT", "1")
            .env("ORBIT_RUN_ID", "jrun-root-selection-test")
            .env("ORBIT_REGISTRY_ROOT", &root);
        command
    };
    assert_selected(&stdio_discovery(&mut managed(), &[]), &expected);
    assert_selected(&listener_discovery(&mut managed()), &expected);

    // A local data-root override cannot select an SSH destination's registry.
    // The stub makes even a regression unable to launch an external session.
    plant_agent_cli_stub(&McpWorkspace::stub_bin_dir(&ambient.home), "ssh");
    for flag in [false, true] {
        let mut command = McpWorkspace::orbit_command(&ambient.work, &ambient.home);
        if flag {
            command.arg("--root").arg(&root);
        } else {
            command.env("ORBIT_ROOT", &root);
        }
        let output = command
            .args(["mcp", "serve", "--mode", "remote", "root-selection-host"])
            .output()
            .expect("run remote root-override refusal");
        assert!(!output.status.success(), "{output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Orbit data root override"), "{error}");
        assert!(error.contains("ORBIT_ROOT"), "{error}");
        assert!(error.contains("SSH destination"), "{error}");
    }
}
