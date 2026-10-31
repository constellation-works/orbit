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
fn claimed_worker_reads_owner_records_and_preserves_workspace_routing() {
    if crate::isolated_test::run_in_child_with_env(
        "mcp_roundtrip::worker_routing::claimed_worker_reads_owner_records_and_preserves_workspace_routing",
        &[("ORBIT_AGENT_MODEL", "codex")],
    ) {
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
    // This routing fixture claims work without a before-landing review lane.
    owner.run_ok(&["config", "set", "review.timing", "off"]);
    std::fs::write(owner.work.join("work.rs"), "fn work() {}\n").expect("task context");
    let (owner_machine_id, _) = machine_identity(&owner.home);
    let mut owner_operator = owner.serve_with_args(&["--operator"]);
    let task = owner_operator.call_tool_ok(
        "orbit_task_add",
        json!({
            "title": "Remote worker route probe",
            "description": "Probe",
            "complexity": "low",
            "model": "codex",
            "context_files": ["file:work.rs"],
        }),
    );
    let task_id = task["id"].as_str().expect("task id");
    owner_operator.call_tool_ok(
        "orbit_task_update",
        json!({"id": task_id, "plan": "Validate remote worker artifact routing"}),
    );
    owner_operator.call_tool_ok(
        "orbit_task_update",
        json!({"id": task_id, "status": "backlog"}),
    );
    let owner_discovery = owner_operator.call_tool_ok("orbit_workspace_list", json!({}));
    let owner_workspace_id = owner_discovery["workspaces"][0]["id"]
        .as_str()
        .expect("owner workspace id");
    let owner_destination = format!("{owner_machine_id}/{owner_workspace_id}");
    drop(owner_operator);

    // A second workspace on the same owner must never leak into claimed reads.
    let other_work = owner.home.join("other-work");
    std::fs::create_dir_all(&other_work).expect("other owner workspace");
    assert!(
        crate::git_repo::command()
            .args(["init", "--quiet"])
            .current_dir(&other_work)
            .status()
            .expect("initialize other workspace Git")
            .success()
    );
    orbit_ok(McpWorkspace::orbit_command(&other_work, &owner.home).args([
        "workspace",
        "init",
        "--name",
        "other-owner-workspace",
    ]));
    orbit_ok(
        McpWorkspace::orbit_command(&other_work, &owner.home).args([
            "tool",
            "run",
            "orbit.task.add",
            "--input",
            &json!({"title": "Remote worker route probe", "description": "Other workspace record",
                "complexity": "low", "workspace": other_work, "model": "codex"})
            .to_string(),
        ]),
    );

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

    // Exercise prefix-routed CLI preparation before registering the worker.
    let scratch = worker.work.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch).expect("worker scratch");
    for (source, sibling, artifact_path) in [
        (
            "cli-source.txt ",
            Some("cli-source.txt"),
            "evidence/cli-exact.txt",
        ),
        ("cli-lone.txt ", None, "evidence/cli-lone.txt"),
    ] {
        std::fs::write(scratch.join(source), b"exact CLI bytes\n").expect("exact source");
        if let Some(sibling) = sibling {
            std::fs::write(scratch.join(sibling), b"wrong sibling bytes\n")
                .expect("trimmed sibling");
        }
        orbit_ok(
            McpWorkspace::orbit_command(&worker.work, &worker.home).args([
                "task",
                "artifact",
                "put",
                task_id,
                &format!(".orbit/tmp/{source}"),
                "--path",
                &format!(" {artifact_path} "),
                "--model",
                "codex",
            ]),
        );
        let mut owner_client = owner.serve();
        let read = owner_client.call_tool_ok(
            "orbit_task_artifact_get",
            json!({"id": task_id, "path": artifact_path}),
        );
        assert_eq!(read["content"], "exact CLI bytes\n", "{source:?}");
    }

    // Artifact reads and writes require a live owner claim, unlike task.show.
    use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
    let path = stub_first_path(&McpWorkspace::stub_bin_dir(&worker.home));
    let _path = test_env::scoped([("PATH", Some(path.to_str().expect("fixture PATH")))]);
    let follower = FederatedMcpHost::new(
        vec![Destination::ssh("owner-ssh", &owner_machine_id)],
        std::sync::Arc::new(SshDestinationProbe::new(
            worker_machine_id.clone(),
            RESPONSE_TIMEOUT,
            RESPONSE_TIMEOUT,
            None,
            orbit_mcp::McpSessionAuthority::Agent,
        )),
    );
    let call = |name: &str, mut input: Value| {
        input["workspace"] = json!(owner_destination);
        follower
            .call_internal_drain(name, input, Default::default())
            .expect("owner claim protocol")
    };
    let probe = call("orbit.drain.probe", json!({}));
    let admitted = call(
        "orbit.task.pull",
        json!({
            "request_id": "worker-artifact-admission", "caller_version": probe["binary_version"],
            "caller_schema": probe["protocol_schema"], "caller_before_pr": false,
            "run_context": {"run_id": "worker-drain", "job_name": "workspace_pull_pipeline"},
            "ship": probe["ship"],
        }),
    );
    assert_eq!(
        admitted["receipt"]["claim"]["task_id"], task_id,
        "{admitted}"
    );
    let claim_id = admitted["receipt"]["claim"]["claim_id"]
        .as_str()
        .expect("claim id");
    call(
        "orbit.drain.claim.bind",
        json!({
            "claim_id": claim_id, "run_id": "run-test-1", "ship": probe["ship"],
        }),
    );

    let binding = orbit_types::tool::WorkerInvocation {
        owner_machine_id: owner_machine_id.clone(),
        owner_workspace_id: owner_workspace_id.to_string(),
        owner_destination: owner_destination.clone(),
        task_id: task_id.to_string(),
        claim_id: claim_id.into(),
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

    // Admitted over the real drain protocol, these reads must return owner
    // records rather than the empty worker store or another owner workspace.
    let listed = client.call_tool_ok("orbit_task_list", json!({"fields": ["id", "title"]}));
    assert_eq!(
        listed["tasks"],
        json!([{"id": task_id, "title": "Remote worker route probe"}])
    );
    let found = client.call_tool_ok(
        "orbit_search",
        json!({"query": "Remote worker route probe", "kind": "task"}),
    );
    assert_eq!(found["results"][0]["id"], task_id, "{found}");
    assert_eq!(
        found["results"].as_array().expect("search results").len(),
        1,
        "only the claimed workspace: {found}"
    );
    for (tool, input, expected) in [
        (
            "orbit.task.list",
            json!({"fields": ["id", "title"]}),
            listed.clone(),
        ),
        (
            "orbit.search",
            json!({"query": "Remote worker route probe", "kind": "task"}),
            found.clone(),
        ),
    ] {
        let output = orbit_ok(
            McpWorkspace::orbit_command(&worker.work, &worker.home).args([
                "tool",
                "run",
                tool,
                "--input",
                &input.to_string(),
            ]),
        );
        let actual: Value = serde_json::from_slice(&output.stdout).expect("CLI owner response");
        assert_eq!(
            actual, expected,
            "{tool}: CLI and MCP reach the same owner records"
        );
    }
    for input in [
        json!({"all_workspaces": true}),
        json!({"allWorkspaces": "true"}),
        json!({"workspaces": ["ws_other"]}),
        json!({"workspace_scope": [owner_workspace_id]}),
    ] {
        let mut input = input;
        input["query"] = json!("Remote worker route probe");
        let refused = client.call_tool_err("orbit_search", input);
        assert_eq!(refused["code"], "policy_denied", "{refused}");
    }
    for tool in ["orbit_search", "orbit_task_list"] {
        let refused =
            client.call_tool_err(tool, json!({"workspace": "ws_other", "query": "probe"}));
        // Workspace-required tools may reject an unknown selector before the
        // claim binding is resolved; neither refusal may return foreign records.
        assert!(
            matches!(
                refused["code"].as_str(),
                Some("policy_denied" | "invalid_input")
            ),
            "{refused}"
        );
    }

    // The worker reads the source before forwarding bytes to its remote owner.
    // MCP must trim even when an exact trailing-space sibling also exists.
    for (source, sibling, artifact_path) in [
        ("source.txt", false, "evidence/trimmed-only.txt"),
        ("siblings.txt", true, "evidence/trimmed-sibling.txt"),
    ] {
        std::fs::write(scratch.join(source), b"trimmed MCP bytes\n").expect("trimmed source");
        if sibling {
            std::fs::write(scratch.join(format!("{source} ")), b"exact CLI bytes\n")
                .expect("trailing-space sibling");
        }
        client.call_tool_ok(
            "orbit_task_artifact_put",
            json!({
                "id": task_id, "source_path": format!(".orbit/tmp/{source} "),
                "path": format!(" {artifact_path} "), "model": "codex",
            }),
        );
        let read = client.call_tool_ok(
            "orbit_task_artifact_get",
            json!({"id": task_id, "path": artifact_path}),
        );
        assert_eq!(read["content"], "trimmed MCP bytes\n", "{source}");
    }

    // Preparation still refuses workspace escapes and over-limit reads before
    // forwarding an attachment, even with a live owner claim.
    let outside = worker.home.join("outside.txt");
    std::fs::write(&outside, b"outside bytes").expect("outside source");
    std::os::unix::fs::symlink(&outside, scratch.join("escape.txt")).expect("escape symlink");
    std::fs::File::create(scratch.join("oversized.txt"))
        .expect("oversized source")
        .set_len(orbit_types::task::MAX_TASK_ARTIFACT_CONTENT_BYTES + 1)
        .expect("oversized length");
    let mut owner_client = owner.serve();
    let before = owner_client.call_tool_ok(
        "orbit_task_show",
        json!({"id": task_id, "fields": ["artifacts"]}),
    );
    for source in [
        outside.to_str().expect("outside path"),
        ".orbit/tmp/escape.txt ",
        ".orbit/tmp/oversized.txt ",
    ] {
        let refused = client.call_tool_err(
            "orbit_task_artifact_put",
            json!({"id": task_id, "source_path": source, "path": "evidence/refused.txt"}),
        );
        assert_eq!(refused["code"], "invalid_input", "{source}: {refused}");
    }
    assert_eq!(
        owner_client.call_tool_ok(
            "orbit_task_show",
            json!({"id": task_id, "fields": ["artifacts"]})
        ),
        before,
        "refused source preparation must not change owner artifacts"
    );
    drop(owner_client);

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
