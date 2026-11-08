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

const PROGRAM_BACKEND: &str = r#"#!/usr/bin/env python3
import json
import os
import shutil
import sys

def report():
    os.fstat(int(os.environ["ORBIT_PLUGIN_CALLBACK_FD"]))
    return {
        "PATH": os.environ.get("PATH", ""),
        "myprog": shutil.which("myprog") or "",
        "otherprog": shutil.which("otherprog") or "",
        "goneprog": shutil.which("goneprog") or "",
        "retargetedprog": shutil.which("retargetedprog") or "",
        "unresolvedprog": shutil.which("unresolvedprog") or "",
    }

def send(value):
    print(json.dumps(value), flush=True)

if sys.argv[1] == "exec":
    json.load(sys.stdin)
    send({"ok": True, "output": report()})
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
            result = {"content": [], "structuredContent": report()}
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
                    agent_callable: false,
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

#[test]
fn granted_program_directories_are_put_on_child_path_with_restricted_caller_path() {
    let temp = tempfile::tempdir().expect("fixture root");
    let root = temp.path().canonicalize().expect("physical root");
    let command = root.join("backend.py");
    std::fs::write(&command, PROGRAM_BACKEND).expect("write backend");
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
        .expect("executable backend");
    let program_dir = root.join("program-bin");
    std::fs::create_dir(&program_dir).expect("program dir");
    for name in ["myprog", "otherprog"] {
        let program = program_dir.join(name);
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("write program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("executable program");
    }
    let program_dir = program_dir.canonicalize().expect("canonical program dir");
    let retarget_dir = root.join("retarget-bin");
    std::fs::create_dir(&retarget_dir).expect("retarget dir");
    std::os::unix::fs::symlink(
        program_dir.join("myprog"),
        retarget_dir.join("retargetedprog"),
    )
    .expect("retargeted program link");
    let retarget_dir = retarget_dir.canonicalize().expect("canonical retarget dir");
    let gone_dir = root.join("gone-bin");
    let grants = PluginGrantSet::from_grants([PluginGrant::Unsandboxed]);
    for transport in ["exec", "mcp"] {
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
            sandbox: PluginSandbox::None,
            permissions: PluginPermissions::default(),
            programs: vec![
                "myprog".into(),
                "otherprog".into(),
                "goneprog".into(),
                "retargetedprog".into(),
                "unresolvedprog".into(),
            ],
            program_paths: std::collections::BTreeMap::from([
                ("myprog".into(), program_dir.join("myprog")),
                ("otherprog".into(), program_dir.join("otherprog")),
                ("goneprog".into(), gone_dir.join("goneprog")),
                ("retargetedprog".into(), retarget_dir.join("retargetedprog")),
            ]),
            config: Default::default(),
            grants: grants.clone(),
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
            description: "report program resolution".into(),
            parameters: Vec::new(),
            input_schema: None,
            execution_kind: PluginExecutionKind::ReadOnly,
            output_schema: None,
            binding: Arc::new(PluginToolBinding {
                provenance: spec.provenance.clone(),
                execution_kind: PluginExecutionKind::ReadOnly,
                agent_callable: false,
                diagnostic: None,
            }),
            backend,
        };
        let parent = vec![
            ("HOME".into(), root.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let ctx = ToolContext {
            cwd: Some(root.to_string_lossy().into_owned()),
            workspace_root: Some(root.clone()),
            proc_spawn_environment: Some(parent),
            ..Default::default()
        };
        let env = tool.execute(&ctx, json!({})).expect("child environment");
        let expected_path = format!("{}:/usr/bin:/bin", program_dir.display());
        assert_eq!(
            env["PATH"], expected_path,
            "{transport}: granted dir prepended once"
        );
        assert_eq!(
            env["myprog"],
            program_dir.join("myprog").to_string_lossy().as_ref(),
            "{transport}: declared program resolves by name"
        );
        assert_eq!(
            env["otherprog"],
            program_dir.join("otherprog").to_string_lossy().as_ref(),
            "{transport}: second program in same dir resolves"
        );
        assert_eq!(
            env["goneprog"], "",
            "{transport}: missing program adds nothing"
        );
        assert_eq!(
            env["retargetedprog"], "",
            "{transport}: retargeted program adds nothing"
        );
        assert_eq!(
            env["unresolvedprog"], "",
            "{transport}: unresolved program adds nothing"
        );
    }
}
