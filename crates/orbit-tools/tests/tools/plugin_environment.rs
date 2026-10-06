//! Plugin environment admission through both public execution transports.
//! Guards the supplied-caller credential leak and the legacy authority-name
//! exclusion (ORB-14256, ORB-12768).

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use orbit_tools::plugin::{
    McpBackend, McpExpectedTool, PluginBackend, PluginBackendSpec, PluginTool, PluginToolBinding,
};
use orbit_tools::{Tool, ToolContext};
use orbit_types::plugin::{
    PluginExecutionKind, PluginGrant, PluginGrantSet, PluginPermissions, PluginProvenance,
    PluginSandbox,
};
use serde_json::{Value, json};

// The child reports the environment it actually received. The exec and MCP
// protocols exercise their real launch paths, including callback stamping.
const BACKEND: &str = r#"#!/usr/bin/env python3
import json
import os
import sys

def environment():
    os.fstat(int(os.environ["ORBIT_PLUGIN_CALLBACK_FD"]))
    return dict(os.environ)

def send(value):
    print(json.dumps(value), flush=True)

if sys.argv[1] == "exec":
    json.load(sys.stdin)
    send({"ok": True, "output": environment()})
else:
    for line in sys.stdin:
        request = json.loads(line)
        method = request["method"]
        if method == "initialize":
            result = {"protocolVersion": request["params"]["protocolVersion"],
                      "capabilities": {"tools": {}},
                      "serverInfo": {"name": "env-fixture", "version": "1.0.0"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "environment", "inputSchema": {"type": "object"}}]}
        elif method == "tools/call":
            result = {"content": [], "structuredContent": environment()}
        else:
            continue
        send({"jsonrpc": "2.0", "id": request["id"], "result": result})
"#;

#[test]
fn caller_credentials_require_a_plugin_env_pass_request_and_grant_for_both_transports() {
    // A value available to the host but omitted by the caller's policy must
    // not be recovered from ambient state when an explicit snapshot exists.
    let _ambient =
        orbit_common::test_env::scoped([("DATABASE_URL", Some("synthetic-host-database"))]);
    let temp = tempfile::tempdir().expect("fixture root");
    let root = temp.path().canonicalize().expect("physical root");
    let command = root.join("backend.py");
    std::fs::write(&command, BACKEND).expect("write backend");
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
        .expect("executable backend");

    let baseline = [
        ("HOME", root.to_string_lossy().into_owned()),
        ("LANG", "C".into()),
        ("LC_ALL", "C".into()),
        ("LOGNAME", "fixture-user".into()),
        ("PATH", std::env::var("PATH").expect("interpreter PATH")),
        ("SHELL", "/bin/sh".into()),
        ("TERM", "dumb".into()),
        ("TMPDIR", root.to_string_lossy().into_owned()),
        ("TZ", "UTC".into()),
        ("USER", "fixture-user".into()),
    ];
    let requested = [
        "DATABASE_URL",
        "ORBIT_OPERATOR",
        "ORBIT_WORKSPACE_CLAIM_TOKEN",
    ];
    for transport in ["exec", "mcp"] {
        for (request, grant, caller_admits_database) in [
            (false, false, true),
            (false, true, true),
            (true, false, true),
            (true, true, true),
            (true, true, false),
        ] {
            let mut grants = PluginGrantSet::from_grants([PluginGrant::Unsandboxed]);
            if grant {
                grants =
                    PluginGrantSet::from_grants([PluginGrant::Unsandboxed, PluginGrant::EnvPass]);
            }
            let spec = Arc::new(PluginBackendSpec {
                provenance: PluginProvenance {
                    name: "env-fixture".into(),
                    version: "1.0.0".into(),
                    manifest_digest: "fixture".into(),
                    grants: grants.to_recorded(),
                },
                plugin_root: root.clone(),
                state_dir: root.join("state"),
                global_root: root.join("global"),
                command: command.clone(),
                args: vec![transport.into()],
                timeout_ms: Some(5_000),
                // Environment admission is independent of kernel sandbox
                // availability; the fixture holds the explicit opt-out grant.
                sandbox: PluginSandbox::None,
                permissions: PluginPermissions {
                    // Construct a legacy spec directly so excluded authority
                    // names reach the execution boundary despite validation.
                    env_pass: if request {
                        requested.iter().map(|name| (*name).into()).collect()
                    } else {
                        Vec::new()
                    },
                    ..Default::default()
                },
                programs: Vec::new(),
                program_paths: Default::default(),
                config: Default::default(),
                grants,
                secrets: Default::default(),
            });
            let backend = match transport {
                "exec" => PluginBackend::Exec(Arc::clone(&spec)),
                "mcp" => PluginBackend::Mcp(Arc::new(McpBackend::new(
                    Arc::clone(&spec),
                    vec![McpExpectedTool {
                        verb: "environment".into(),
                        input_schema: None,
                    }],
                ))),
                _ => unreachable!(),
            };
            let tool = PluginTool {
                name: "env-fixture.environment".into(),
                verb: "environment".into(),
                description: "report synthetic environment".into(),
                parameters: Vec::new(),
                input_schema: None,
                execution_kind: PluginExecutionKind::ReadOnly,
                output_schema: None,
                binding: Arc::new(PluginToolBinding {
                    provenance: spec.provenance.clone(),
                    execution_kind: PluginExecutionKind::ReadOnly,
                    diagnostic: None,
                }),
                backend,
            };
            let mut parent: Vec<_> = baseline
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect();
            parent.extend(
                [
                    ("OPENAI_API_KEY", "synthetic-provider-credential"),
                    ("INTERNAL_SERVICE_URL", "synthetic-unrequested-endpoint"),
                    ("ORBIT_OPERATOR", "1"),
                    ("ORBIT_WORKSPACE_CLAIM_TOKEN", "synthetic-claim-token"),
                    ("ORBIT_RUN_ID", "fixture-run"),
                    ("ORBIT_ACTIVITY_ID", "fixture-activity"),
                    ("ORBIT_TOOL_NAME", "caller.tool"),
                    ("ORBIT_ALLOWED_TOOLS", "caller-authority"),
                    ("ORBIT_PROC_ALLOWED_PROGRAMS", "caller-program"),
                ]
                .map(|(key, value)| (key.to_string(), value.to_string())),
            );
            if caller_admits_database {
                parent.push(("DATABASE_URL".into(), "synthetic-caller-database".into()));
            }
            let ctx = ToolContext {
                cwd: Some(root.to_string_lossy().into_owned()),
                workspace_root: Some(root.clone()),
                proc_spawn_environment: Some(parent),
                ..Default::default()
            };
            let env = tool.execute(&ctx, json!({})).expect("child environment");
            let case = format!(
                "{transport}: request={request}, grant={grant}, caller_admits={caller_admits_database}"
            );
            for (key, value) in &baseline {
                assert_eq!(env[key], *value, "{case}: baseline {key}");
            }
            for key in [
                "OPENAI_API_KEY",
                "INTERNAL_SERVICE_URL",
                "ORBIT_OPERATOR",
                "ORBIT_WORKSPACE_CLAIM_TOKEN",
            ] {
                assert!(
                    env.get(key).is_none(),
                    "{case}: unrequested credentials and excluded authority must not reach the child ({key})"
                );
            }
            assert_eq!(
                env.get("DATABASE_URL"),
                (request && grant && caller_admits_database)
                    .then_some(&Value::String("synthetic-caller-database".into())),
                "{case}: env_pass requires both plugin consent and caller admission"
            );
            for (key, value) in [
                ("ORBIT_HOST_API", "1"),
                ("ORBIT_VERSION", env!("CARGO_PKG_VERSION")),
                ("ORBIT_PLUGIN", "env-fixture"),
                ("ORBIT_PLUGIN_VERSION", "1.0.0"),
                ("ORBIT_RUN_ID", "fixture-run"),
                ("ORBIT_ACTIVITY_ID", "fixture-activity"),
                ("ORBIT_ALLOWED_TOOLS", ""),
                ("ORBIT_PROC_ALLOWED_PROGRAMS", ""),
                ("ORBIT_PLUGIN_CALLBACK_FD", "3"),
            ] {
                assert_eq!(env[key], value, "{case}: runtime envelope {key}");
            }
            for key in [
                "ORBIT_PLUGIN_ROOT",
                "ORBIT_TOOL_CWD",
                "ORBIT_WORKSPACE_ROOT",
            ] {
                assert_eq!(env[key], root.to_string_lossy().as_ref(), "{case}: {key}");
            }
            assert_eq!(
                env["ORBIT_PLUGIN_STATE"],
                spec.state_dir.to_string_lossy().as_ref(),
                "{case}: plugin state"
            );
            assert_eq!(
                env.get("ORBIT_TOOL_NAME"),
                (transport == "exec").then_some(&Value::String(tool.name.clone())),
                "{case}: only the per-call exec child has a tool name"
            );
        }
    }
}
