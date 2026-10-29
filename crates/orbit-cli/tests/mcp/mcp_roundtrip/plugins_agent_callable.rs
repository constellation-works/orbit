//! Regression for consented append/proposal tools refused to agent sessions.
use super::*;

#[test]
fn consented_mutations_reach_agent_cli_and_mcp_without_operator_authority() {
    if !orbit_exec::macos_sandbox_test_guard(
        "consented_mutations_reach_agent_cli_and_mcp_without_operator_authority",
    ) {
        return;
    }
    for scope in ["workspace", "global"] {
        let workspace = McpWorkspace::init();
        let root = write_plugin_with_scope(&workspace.home, "agentfixture", scope);
        let manifest_path = root.join("plugin.yaml");
        let original = std::fs::read_to_string(&manifest_path).expect("manifest");
        let manifest = original.replace(
            "execution_kind: read_only",
            "execution_kind: mutating\n      agent_callable: true",
        ) + &format!(
            "    - name: destroy\n      execution_kind: mutating\n      mcp_scope: {scope}\n"
        );
        std::fs::write(&manifest_path, &manifest).expect("declare append tool");
        run_orbit(&workspace, &["plugin", "add", root.to_str().expect("root")]);

        let inactive = run_orbit(&workspace, &["plugin", "enable", "agentfixture", "--json"]);
        assert_eq!(stdout_json(&inactive)["status"], "inactive");
        let enabled = run_orbit(
            &workspace,
            &["plugin", "enable", "agentfixture", "--grant", "agent_tools"],
        );
        let text = String::from_utf8_lossy(&enabled.stdout);
        assert!(text.contains("agentfixture.echo"), "{text}");
        let listed = run_orbit(&workspace, &["plugin", "list"]);
        assert!(String::from_utf8_lossy(&listed.stdout).contains("agentfixture.echo"));
        let shown = run_orbit(&workspace, &["plugin", "show", "agentfixture", "--json"]);
        let shown = stdout_json(&shown);
        assert_eq!(shown["tools"][0]["agent_callable"], true);
        let request = shown["permissions"]
            .as_array()
            .expect("permissions")
            .iter()
            .find(|p| p["grant"] == "agent_tools")
            .expect("agent consent");
        assert_eq!(request["requested"], "echo");
        assert_eq!(request["granted"], true);

        let called = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .env("ORBIT_AGENT_NAME", "codex")
            .args([
                "tool",
                "run",
                "agentfixture.echo",
                "--input",
                "{}",
                "--json",
            ])
            .output()
            .expect("agent CLI call");
        assert!(called.status.success(), "{called:?}");
        assert_eq!(stdout_json(&called)["echo"]["tool"], "agentfixture.echo");
        let denied = workspace.run(&[
            "tool",
            "run",
            "agentfixture.destroy",
            "--input",
            "{}",
            "--json",
        ]);
        assert!(!denied.status.success(), "{denied:?}");
        assert!(String::from_utf8_lossy(&denied.stderr).contains("capability_denied"));

        let mut client = if scope == "global" {
            let child = McpWorkspace::orbit_command(&workspace.home, &workspace.home)
                .args(["mcp", "serve"])
                .spawn_piped();
            McpClient::initialized(child, McpClient::initialize_params("agent-plugin", None)).0
        } else {
            workspace.serve()
        };
        let called = client.call_tool_ok("agentfixture_echo", json!({}));
        assert_eq!(called["echo"]["tool"], "agentfixture.echo");
        client.assert_tool_error("agentfixture_destroy", json!({}), "capability_denied");
        drop(client);
        let rows = audit_rows(&workspace, &["--tool", "agentfixture.echo"]);
        let rows = rows.as_array().expect("audit rows");
        for subcommand in ["run", "run-mcp"] {
            let row = rows
                .iter()
                .find(|row| row["subcommand"] == subcommand)
                .expect("audited tool call");
            assert_eq!(row["status"], "success", "{row}");
            if subcommand == "run-mcp" {
                assert_eq!(row["effective_capabilities"], json!(["agent"]), "{row}");
            } else {
                // CLI attribution comes from the process agent envelope;
                // effective_capabilities records trusted session grants only.
                assert_eq!(row["role"], "codex", "{row}");
            }
        }

        // Agent access does not override a managed activity's policy.
        for policy in [
            vec![("ORBIT_ACTIVITY_TOOLS", "orbit.search")],
            vec![
                ("ORBIT_ACTIVITY_TOOL_POLICY", "deny"),
                ("ORBIT_ACTIVITY_TOOLS_DENY", "agentfixture.*"),
                ("ORBIT_ACTIVITY_NAME", "append-policy"),
            ],
        ] {
            let mut env = vec![
                ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
                ("ORBIT_RUN_ID", "jrun-agent-plugin-policy-test"),
                ("ORBIT_TASK_ACTOR_KIND", "agent"),
            ];
            env.extend(policy);
            let mut managed = workspace.serve_with_args_and_env(&[], &env);
            managed.assert_tool_error("agentfixture_echo", json!({}), "policy_denied");
        }

        // A grant already held for echo must not authorize a newly marked tool.
        let widened = manifest
            .replace("version: 0.1.0", "version: 0.2.0")
            .replace(
                "name: destroy\n",
                "name: destroy\n      agent_callable: true\n",
            );
        std::fs::write(&manifest_path, widened).expect("widen declaration");
        let upgraded = run_orbit(
            &workspace,
            &[
                "plugin",
                "upgrade",
                "agentfixture",
                root.to_str().expect("root"),
                "--json",
            ],
        );
        let upgraded = stdout_json(&upgraded);
        assert_eq!(upgraded["status"], "disabled", "{upgraded}");
        assert_eq!(upgraded["granted"], json!([]), "{upgraded}");
        assert!(
            upgraded["permission_changes"]
                .as_array()
                .expect("permission changes")
                .iter()
                .any(|change| change["grant"] == "agent_tools" && change["widened"] == true),
            "{upgraded}"
        );

        // The declaration is invalid on read-only tools, rather than ignored.
        std::fs::write(
            &manifest_path,
            manifest.replacen("execution_kind: mutating", "execution_kind: read_only", 1),
        )
        .expect("invalid declaration");
        let invalid = workspace.run(&["plugin", "validate", root.to_str().expect("root")]);
        assert!(!invalid.status.success());
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("agent_callable"));
    }
}
