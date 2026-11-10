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
        "aliasprog": shutil.which("aliasprog") or "",
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
        // The host's link directory, one link per granted name, not the
        // recorded files' parent: a name reached through a link is not there.
        let link_dir = root
            .join("global")
            .join("plugins")
            .join(".programs")
            .join("env-fixture");
        let expected_path = format!("{}:/usr/bin:/bin", link_dir.display());
        assert_eq!(
            env["PATH"], expected_path,
            "{transport}: granted link dir prepended once"
        );
        assert_eq!(
            env["myprog"],
            link_dir.join("myprog").to_string_lossy().as_ref(),
            "{transport}: declared program resolves by name"
        );
        assert_eq!(
            env["otherprog"],
            link_dir.join("otherprog").to_string_lossy().as_ref(),
            "{transport}: second program resolves through its own link"
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

/// A CLI reached through a symlink whose target basename is not the declared
/// name (`~/.local/bin/claude` -> `…/versions/2.1.318`). The child's `PATH`
/// neither names the symlink's directory nor the target's, yet the declared
/// name still runs the recorded file, and the link never follows a new target.
#[test]
fn declared_program_reached_through_symlink_runs_recorded_file_by_name() {
    let temp = tempfile::tempdir().expect("fixture root");
    let root = temp.path().canonicalize().expect("physical root");
    let command = root.join("backend.py");
    std::fs::write(&command, PROGRAM_BACKEND).expect("write backend");
    std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))
        .expect("executable backend");
    let versions = root.join("versions");
    std::fs::create_dir(&versions).expect("versions dir");
    let recorded = versions.join("2.1.318");
    std::fs::write(&recorded, "#!/bin/sh\necho ran-recorded\n").expect("write recorded program");
    std::fs::set_permissions(&recorded, std::fs::Permissions::from_mode(0o755))
        .expect("executable recorded program");
    let linked_dir = root.join("linkbin");
    std::fs::create_dir(&linked_dir).expect("link dir");
    std::os::unix::fs::symlink(&recorded, linked_dir.join("aliasprog")).expect("alias link");
    let grants = PluginGrantSet::from_grants([PluginGrant::Unsandboxed]);
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
        args: vec!["exec".into()],
        timeout_ms: Some(5_000),
        sandbox: PluginSandbox::None,
        permissions: PluginPermissions::default(),
        programs: vec!["aliasprog".into()],
        program_paths: std::collections::BTreeMap::from([("aliasprog".into(), recorded.clone())]),
        config: Default::default(),
        grants: grants.clone(),
        secrets: Default::default(),
    });
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
        backend: PluginBackend::Exec(Arc::clone(&spec)),
    };
    let ctx = ToolContext {
        cwd: Some(root.to_string_lossy().into_owned()),
        workspace_root: Some(root.clone()),
        proc_spawn_environment: Some(vec![
            ("HOME".into(), root.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        ..Default::default()
    };
    let env = tool.execute(&ctx, json!({})).expect("child environment");

    let child_path = env["PATH"].as_str().expect("child PATH");
    let path_dirs: Vec<&str> = child_path.split(':').collect();
    assert!(
        !path_dirs.contains(&linked_dir.to_string_lossy().as_ref())
            && !path_dirs.contains(&versions.to_string_lossy().as_ref()),
        "the child PATH must not depend on the symlink's or the target's directory: {child_path}"
    );
    let link = root
        .join("global")
        .join("plugins")
        .join(".programs")
        .join("env-fixture")
        .join("aliasprog");
    assert_eq!(
        env["aliasprog"],
        link.to_string_lossy().as_ref(),
        "the declared name resolves to its program link"
    );
    assert_eq!(
        link.canonicalize().expect("link target"),
        recorded,
        "the link resolves to exactly the recorded, granted file"
    );
    let output = std::process::Command::new(&link)
        .output()
        .expect("run the declared program by its link");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ran-recorded",
        "the declared name runs the recorded file"
    );

    // Retargeting the CLI after consent: the recorded path is still what the
    // grant names, and the new target is reported, never followed.
    let upgraded = versions.join("2.1.322");
    std::fs::write(&upgraded, "#!/bin/sh\necho ran-upgraded\n").expect("write upgraded program");
    std::fs::set_permissions(&upgraded, std::fs::Permissions::from_mode(0o755))
        .expect("executable upgraded program");
    std::fs::remove_file(linked_dir.join("aliasprog")).expect("remove old alias");
    std::os::unix::fs::symlink(&upgraded, linked_dir.join("aliasprog")).expect("retarget alias");
    let env = tool
        .execute(&ctx, json!({}))
        .expect("child environment after retarget");
    assert_eq!(
        env["aliasprog"],
        link.to_string_lossy().as_ref(),
        "the grant still resolves the recorded file's link after a retarget"
    );
    let output = std::process::Command::new(&link)
        .output()
        .expect("run the declared program after retarget");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ran-recorded",
        "a changed symlink target is never followed silently"
    );
}

/// Inspection (`orbit plugin show`/`doctor`) reports a bare name that now
/// reaches a different file than the recorded one (an auto-updated CLI), and
/// keeps granting the recorded file. A name that resolves to the recorded file
/// or not at all is not drift.
#[test]
fn bare_program_name_reaching_another_file_is_reported_and_not_followed() {
    let temp = tempfile::tempdir().expect("fixture root");
    let root = temp.path().canonicalize().expect("physical root");
    let versions = root.join("versions");
    std::fs::create_dir(&versions).expect("versions dir");
    let write_program = |path: &std::path::Path| {
        std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write program");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("executable program");
    };
    let recorded = versions.join("2.1.318");
    let upgraded = versions.join("2.1.322");
    write_program(&recorded);
    write_program(&upgraded);
    let bin = root.join("bin");
    std::fs::create_dir(&bin).expect("bin dir");
    let link = bin.join("claude");
    std::os::unix::fs::symlink(&recorded, &link).expect("cli link");
    let global = root.join("global");
    let state = root.join("state");
    let declared = vec!["claude".to_string(), "goneprog".to_string()];
    let recorded_paths = std::collections::BTreeMap::from([
        ("claude".to_string(), recorded.clone()),
        ("goneprog".to_string(), root.join("gone").join("goneprog")),
    ]);
    let search = Some(bin.as_os_str());

    let mut statuses =
        orbit_tools::plugin::program_statuses(&declared, &recorded_paths, &global, &state);
    orbit_tools::plugin::mark_resolution_drift(&mut statuses, search);
    assert!(statuses[0].granted(), "the recorded file is granted");
    assert_eq!(
        statuses[0].resolves_to, None,
        "a name that still reaches the recorded file is not drift"
    );
    assert_eq!(
        statuses[1].resolves_to, None,
        "a name that does not resolve is not drift"
    );

    std::fs::remove_file(&link).expect("remove cli link");
    std::os::unix::fs::symlink(&upgraded, &link).expect("auto-updated cli link");
    let mut statuses =
        orbit_tools::plugin::program_statuses(&declared, &recorded_paths, &global, &state);
    orbit_tools::plugin::mark_resolution_drift(&mut statuses, search);
    assert_eq!(
        statuses[0].path.as_deref(),
        Some(recorded.as_path()),
        "the grant still names the recorded file"
    );
    assert!(
        statuses[0].granted(),
        "drift does not revoke the recorded grant"
    );
    assert_eq!(
        statuses[0].resolves_to.as_deref(),
        Some(upgraded.as_path()),
        "the name's new file is reported, not followed"
    );
}
