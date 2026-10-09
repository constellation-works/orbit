//! Worker MCP routing to its bound remote owner.

use super::*;

fn plant_transparent_ssh_stub(bin: &Path, owner_home: &Path, owner_work: &Path) {
    plant_agent_cli_stub(bin, "ssh");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, shlex, subprocess, sys
argv = shlex.split(sys.argv[-1])
assert argv.pop(0) == 'orbit'
env = os.environ.copy()
env['HOME'] = {home}
env['USERPROFILE'] = {home}
sys.exit(subprocess.call([{binary}] + argv, cwd={work}, env=env, stderr=subprocess.DEVNULL))
"#,
        home = json!(owner_home),
        work = json!(owner_work),
        binary = json!(env!("CARGO_BIN_EXE_orbit")),
    );
    let stub = bin.join("ssh");
    std::fs::write(&stub, script).expect("write transparent ssh stub");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("chmod transparent ssh stub");
}

#[test]
fn mcp_worker_remote_owner_route_treats_null_and_blank_workspace_as_absent() {
    const CHILD_ENV: &str = "ORBIT_TEST_WORKER_REMOTE_OWNER_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        let temp = tempdir().expect("isolated worker home");
        let mut child = Command::new(std::env::current_exe().expect("integration binary"));
        test_env::clear_inherited_authority(|name| {
            child.env_remove(name);
        });
        child
            .current_dir(temp.path())
            .env("HOME", temp.path())
            .env("USERPROFILE", temp.path())
            .env("ORBIT_AGENT_MODEL", "codex")
            .env(CHILD_ENV, "1")
            .args([
                "--exact",
                "mcp_roundtrip::worker_routing::mcp_worker_remote_owner_route_treats_null_and_blank_workspace_as_absent",
                "--nocapture",
            ]);
        let output = assert_cmd::Command::from_std(child)
            .timeout(RESPONSE_TIMEOUT)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        assert!(
            String::from_utf8_lossy(&output).contains("1 passed"),
            "child test `mcp_roundtrip::worker_routing::mcp_worker_remote_owner_route_treats_null_and_blank_workspace_as_absent` did not execute: {}",
            String::from_utf8_lossy(&output)
        );
        return;
    }

    struct DummyOwner;
    impl orbit_tools::OwnerCoordinator for DummyOwner {
        fn call(
            &self,
            _name: &str,
            _input: Value,
            _session: orbit_types::tool::ToolSessionContext,
        ) -> Result<Value, orbit_common::OrbitError> {
            unreachable!()
        }
    }

    let owner = McpWorkspace::init();
    let (owner_machine_id, _) = machine_identity(&owner.home);
    let mut owner_operator = owner.serve_with_args(&["--operator"]);
    let task = owner_operator.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "Remote worker route probe",
            "description": "Probe",
            "complexity": "low",
            "model": "codex",
        }),
    );
    let task_id = task["id"].as_str().expect("task id");
    let owner_discovery = owner_operator.call_tool_ok("orbit_workspace_list", json!({}));
    let owner_workspace_id = owner_discovery["workspaces"][0]["id"]
        .as_str()
        .expect("owner workspace id");
    let owner_destination = format!("{owner_machine_id}/{owner_workspace_id}");
    drop(owner_operator);

    let worker = McpWorkspace::init_with_task_prefix("mcp-roundtrip", &[], "WRK");
    let (worker_machine_id, _) = machine_identity(&worker.home);
    std::fs::write(
        worker.home.join(".orbit").join("hosts.toml"),
        format!(
            "schema_version = 1\n\n[[hosts]]\nname = \"owner-host\"\nmachine_id = \"{owner_machine_id}\"\nssh = \"owner-ssh\"\ntask_prefix = \"TST\"\n"
        ),
    )
    .expect("write hosts.toml");

    plant_transparent_ssh_stub(
        &McpWorkspace::stub_bin_dir(&worker.home),
        &owner.home,
        &owner.work,
    );

    let binding = orbit_types::tool::WorkerInvocation {
        owner_machine_id: owner_machine_id.clone(),
        owner_workspace_id: owner_workspace_id.to_string(),
        owner_destination: owner_destination.clone(),
        task_id: task_id.to_string(),
        claim_id: "claim-test-1".into(),
        execution: orbit_types::task::ExecutionLocation {
            machine_id: worker_machine_id.clone(),
            machine_name: None,
        },
        bound_run_id: "run-test-1".into(),
    };

    let runtime = orbit_core::OrbitRuntime::from_roots(
        &worker.home.join(".orbit"),
        &worker.work.join(".orbit"),
    )
    .expect("open worker runtime")
    .with_worker_invocation(binding, std::sync::Arc::new(DummyOwner))
    .expect("bind worker invocation");

    orbit_engine::RuntimeHost::register_worker_process(&runtime, std::process::id())
        .expect("register worker process");

    let mut client = worker.serve();

    // Absent, null, and blank selectors route to the remote owner destination.
    for absent in [Value::Null, json!(""), json!("   ")] {
        let shown = client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "workspace": absent }),
        );
        assert_eq!(
            shown["id"], task_id,
            "null/blank must route to owner: {shown}"
        );
        assert_eq!(shown["title"], "Remote worker route probe");
    }

    let implicit = client.call_tool_ok("orbit_task_show", json!({ "id": task_id }));
    assert_eq!(implicit["id"], task_id);

    // Explicit valid selectors (matching destination, workspace ID) also succeed.
    for valid in [
        json!(owner_destination),
        json!(owner_workspace_id),
        json!(worker.work),
    ] {
        let shown = client.call_tool_ok(
            "orbit_task_show",
            json!({ "id": task_id, "workspace": valid }),
        );
        assert_eq!(shown["id"], task_id);
    }

    // A non-string selector is refused.
    for invalid in [json!(42), json!(true), json!(["array"])] {
        let refused = client.call_tool_err(
            "orbit_task_show",
            json!({ "id": task_id, "workspace": invalid }),
        );
        assert_eq!(refused["code"], "invalid_input", "{refused}");
        assert!(
            refused["message"]
                .as_str()
                .is_some_and(|m| m.contains("`workspace` must be a string")),
            "must describe invalid input: {refused}"
        );
    }

    // A string naming another workspace is denied as a binding mismatch.
    let denied = client.call_tool_err(
        "orbit_task_show",
        json!({ "id": task_id, "workspace": "ws_other" }),
    );
    assert_eq!(denied["code"], "policy_denied", "{denied}");
    assert!(
        denied["message"]
            .as_str()
            .is_some_and(|m| m.contains("worker workspace binding mismatch")),
        "must deny as binding mismatch: {denied}"
    );
}
