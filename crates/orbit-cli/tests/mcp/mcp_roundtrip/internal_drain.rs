//! Public retirement and the deterministic follower route through real stdio.
use super::*;

const OPERATIONS: &[&str] = &[
    "orbit.drain.probe",
    "orbit.drain.receipt.lookup",
    "orbit.drain.claim.bind",
    "orbit.drain.claim.settle",
    "orbit.task.pull",
];

fn audit_rows(workspace: &McpWorkspace, name: &str) -> Vec<Value> {
    let output = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home)
            .args(["audit", "list", "--tool", name, "--json", "--limit", "100"]),
    );
    serde_json::from_slice::<Value>(&output.stdout)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn public_drain_calls_and_spoofed_initialize_are_refused_and_audited() {
    let workspace = McpWorkspace::init();
    let mut clients = vec![
        workspace.serve(),
        workspace.serve_with_args(&["--operator"]),
        workspace.serve_with_args(&["--mode", "federated"]),
        workspace.serve_with_args_and_env(
            &[],
            &[
                ("ORBIT_MANAGED_RUN_CONTEXT", "1"),
                ("ORBIT_ACTIVITY_TOOLS", "orbit.*"),
                ("ORBIT_AGENT_NAME", "codex"),
            ],
        ),
    ];
    let child = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["mcp", "serve", "--remote-caller-machine-id", "hm_follower"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut params = McpClient::initialize_params(
        "orbit-federated-mux",
        Some(workspace.work.to_str().unwrap()),
    );
    params["_meta"]["orbit"]["internal_drain"] = json!(true);
    params["_meta"]["orbit"]["effective_capabilities"] = json!(["agent", "operator"]);
    let (spoofed, response) = McpClient::initialized(child, params);
    assert!(response.get("error").is_none(), "{response}");
    clients.push(spoofed);
    let baseline = clients[0].call_tool_ok("orbit_task_list", json!({}));
    for client in &mut clients {
        let response = client.request("tools/list", Value::Null);
        let names = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            names.len(),
            25,
            "the reviewed 37-tool surface loses exactly five protocol tools and seven \
             tools folded into their siblings"
        );
        for name in OPERATIONS {
            let advertised = orbit_types::tool::mcp_advertised_tool_name(name);
            assert!(!names.contains(name) && !names.contains(advertised.as_str()));
            for spelling in [*name, advertised.as_str()] {
                assert_eq!(
                    client.call_tool_err(spelling, json!({}))["code"],
                    "policy_denied"
                );
            }
            let refused = client.request(
                "orbit/internal/drain/call",
                json!({"protocol":1,"name":name,"arguments":{}}),
            );
            assert_eq!(refused["result"]["isError"], true, "{refused}");
            assert_eq!(
                refused["result"]["structuredContent"]["code"],
                "policy_denied"
            );
        }
        assert!(names.contains("orbit_task_add") && names.contains("orbit_workflow_ship"));
        let preflight = client.request("orbit/internal/drain/preflight", json!({}));
        assert_eq!(preflight["error"]["code"], -32601);
    }
    assert_eq!(
        clients[0].call_tool_ok("orbit_task_list", json!({})),
        baseline
    );
    drop(clients);
    for name in OPERATIONS {
        let rows = audit_rows(&workspace, name);
        assert_eq!(
            rows.len(),
            15,
            "all public and custom refusals are durable: {rows:?}"
        );
        assert!(rows.iter().all(|row| row["status"] == "denied"), "{rows:?}");
    }
}

/// Run the fixture's Rust transport client in a clean child as well as its
/// server processes: SshDestinationProbe reads the client's PATH and identity.
fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_INTERNAL_DRAIN_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = tempdir().unwrap();
    // libtest names a test by its module path below the crate root.
    let qualified = format!(
        "{}::{test}",
        module_path!().split_once("::").expect("test module").1
    );
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(["--exact", &qualified, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"));
    false
}

#[cfg(unix)]
fn ssh_fixture(owner: &McpWorkspace, bin: &Path, log: &Path, lose_answer: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    // Execute exactly the runtime-composed remote argv, changing only the
    // destination machine's executable and filesystem roots. Drop one committed
    // admission answer to exercise OutcomeUnknown and a new owner process.
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, shlex, subprocess, sys, threading
argv = shlex.split(sys.argv[-1])
assert argv.pop(0) == 'orbit'
env = os.environ.copy()
env['HOME'] = {home}
env['USERPROFILE'] = {home}
child = subprocess.Popen([{binary}] + argv, cwd={work}, env=env,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
def forward():
    for line in sys.stdin.buffer:
        with open({log}, 'ab') as evidence: evidence.write(line)
        child.stdin.write(line)
        child.stdin.flush()
    child.stdin.close()
threading.Thread(target=forward, daemon=True).start()
for line in child.stdout:
    message = json.loads(line)
    receipt = message.get('result', {{}}).get('structuredContent', {{}}).get('receipt')
    if receipt and receipt.get('claim') and os.path.exists({lose}):
        os.rename({lose}, {lose} + '.consumed')
        child.terminate()
        child.wait()
        sys.exit(0)
    sys.stdout.buffer.write(line)
    sys.stdout.buffer.flush()
child.wait()
"#,
        home = json!(owner.home),
        work = json!(owner.work),
        binary = json!(env!("CARGO_BIN_EXE_orbit")),
        log = json!(log),
        lose = json!(lose_answer)
    );
    std::fs::write(bin.join("ssh"), script).unwrap();
    std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[cfg(unix)]
fn follower_internal_transport_reconciles_lost_admission_and_fences_claims() {
    if !isolated("follower_internal_transport_reconciles_lost_admission_and_fences_claims") {
        return;
    }
    use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
    use orbit_types::tool::ToolSessionContext;
    use std::sync::Arc;

    let owner = McpWorkspace::init();
    let mut public = owner.serve_with_args(&["--operator"]);
    let discovery = public.call_tool_ok("orbit_workspace_list", json!({}));
    let machine = discovery["machine_id"].as_str().unwrap();
    let workspace_id = discovery["workspaces"][0]["id"].as_str().unwrap();
    let selector = format!("{machine}/{workspace_id}");
    std::fs::write(owner.work.join("work.rs"), "fn work() {}\n").unwrap();
    let task = public.call_tool_ok("orbit_task_add", json!({"title":"Internal protocol fixture",
        "description":"Disposable claim", "complexity":"low", "model":"codex", "context_files":["file:work.rs"]}));
    let id = task["id"].as_str().unwrap();
    public.call_tool_ok(
        "orbit_task_update",
        json!({"id":id,"plan":"Implement work.rs and validate"}),
    );
    public.call_tool_ok("orbit_task_update", json!({"id":id,"status":"backlog"}));

    let bin = owner._temp.path().join("follower-bin");
    let log = owner._temp.path().join("internal-wire.jsonl");
    let lose = owner._temp.path().join("lose-next-answer");
    ssh_fixture(&owner, &bin, &log, &lose);
    let path = stub_first_path(&bin);
    let _path = test_env::scoped([("PATH", Some(path.to_str().unwrap()))]);
    let host = |caller: &str| {
        FederatedMcpHost::new(
            vec![Destination::ssh("isolated-owner", machine)],
            Arc::new(SshDestinationProbe::new(
                caller.into(),
                Duration::from_secs(30),
                Duration::from_secs(30),
                None,
                orbit_mcp::McpSessionAuthority::Agent,
            )),
        )
    };
    let follower = host("hm_follower");
    let call = |peer: &FederatedMcpHost, name: &str, mut input: Value| {
        input["workspace"] = json!(selector);
        peer.call_internal_drain(name, input, ToolSessionContext::default())
    };
    let probe = call(&follower, "orbit.drain.probe", json!({})).unwrap();
    assert_eq!(probe["owner_machine_id"], machine);
    assert_eq!(probe["session"]["caller_machine_id"], "hm_follower");
    let request = json!({"request_id":"same-admission", "caller_version":probe["binary_version"],
        "caller_schema":1,"caller_review_policy":"none",
        "run_context":{"run_id":"follower-drain","job_name":"workspace_pull_pipeline"},"ship":probe["ship"]});
    std::fs::write(&lose, "drop the next committed reply").unwrap();
    let lost = call(&follower, "orbit.task.pull", request.clone()).unwrap_err();
    assert!(
        matches!(lost, orbit_common::OrbitError::OutcomeUnknown { .. }),
        "{lost}"
    );
    let lookup = call(
        &follower,
        "orbit.drain.receipt.lookup",
        json!({"request_id":"same-admission"}),
    )
    .unwrap();
    assert_eq!(lookup["outcome"], "found", "{lookup}");
    let admitted = call(&follower, "orbit.task.pull", request.clone()).unwrap();
    assert_eq!(admitted["receipt"], lookup["receipt"]);
    assert_eq!(admitted["receipt"]["claim"]["task_id"], id);
    let claim = admitted["receipt"]["claim"]["claim_id"].clone();
    let bind = json!({"claim_id":claim,"run_id":"leaf-1","ship":probe["ship"]});
    assert!(call(&host("hm_wrong"), "orbit.drain.claim.bind", bind.clone()).is_err());
    let bound = call(&follower, "orbit.drain.claim.bind", bind.clone()).unwrap();
    assert_eq!(bound["phase"], "running");
    assert_eq!(
        call(&follower, "orbit.drain.claim.bind", bind.clone()).unwrap(),
        bound
    );
    let mut wrong_run = bind.clone();
    wrong_run["run_id"] = json!("leaf-2");
    assert!(call(&follower, "orbit.drain.claim.bind", wrong_run).is_err());
    let mut stale = bind;
    stale["claim_id"] = json!("stale-claim");
    assert!(
        call(&follower, "orbit.drain.claim.bind", stale)
            .unwrap_err()
            .to_string()
            .contains("stale_claim")
    );
    let mut mismatch = request;
    mismatch["run_context"]["run_id"] = json!("another-drain");
    assert!(
        call(&follower, "orbit.task.pull", mismatch)
            .unwrap_err()
            .to_string()
            .contains("request_mismatch")
    );
    let failure = json!({"claim_id":claim,"run_id":"leaf-1","settlement":{"Fail":{"summary":"Fixture failed","comment":null,"artifacts":[]}}});
    assert!(
        call(
            &host("hm_wrong"),
            "orbit.drain.claim.settle",
            failure.clone()
        )
        .is_err()
    );
    let mut wrong_run = failure.clone();
    wrong_run["run_id"] = json!("leaf-2");
    assert!(call(&follower, "orbit.drain.claim.settle", wrong_run).is_err());
    let settled = call(&follower, "orbit.drain.claim.settle", failure.clone()).unwrap();
    assert_eq!(settled["phase"], "failed");
    assert_eq!(
        call(&follower, "orbit.drain.claim.settle", failure).unwrap(),
        settled
    );
    let reopened = call(
        &follower,
        "orbit.drain.receipt.lookup",
        json!({"request_id":"same-admission"}),
    )
    .unwrap();
    assert_eq!(reopened["receipt"], admitted["receipt"]);
    assert_eq!(reopened["current_claim"]["phase"], "failed");
    assert_eq!(
        public.call_tool_ok("orbit_task_show", json!({"id":id}))["status"],
        "blocked"
    );
    let claims = orbit_ok(
        McpWorkspace::orbit_command(&owner.work, &owner.home)
            .env("ORBIT_OPERATOR", "1")
            .args(["tool", "run", "orbit.drain.claims", "--input", "{}"]),
    );
    let claims: Value = serde_json::from_slice(&claims.stdout).unwrap();
    assert_eq!(
        claims.as_array().unwrap().len(),
        1,
        "one durable admission/effect: {claims}"
    );
    let wrong_workspace = follower
        .call_internal_drain(
            "orbit.drain.probe",
            json!({"workspace":format!("{machine}/ws_missing")}),
            ToolSessionContext::default(),
        )
        .unwrap_err();
    assert!(matches!(
        wrong_workspace,
        orbit_common::OrbitError::StaleRoute(_)
    ));
    for name in OPERATIONS {
        let rows = audit_rows(&owner, name);
        assert!(
            rows.iter().any(|row| row["status"] == "success"),
            "{name}: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row["caller_machine_id"] == "hm_follower"),
            "caller attribution: {rows:?}"
        );
    }
    for name in ["orbit.drain.claim.bind", "orbit.drain.claim.settle"] {
        let rows = audit_rows(&owner, name);
        assert!(
            rows.iter()
                .any(|row| row["status"] == "failure" && row["caller_machine_id"] == "hm_wrong"),
            "foreign refusal attribution: {rows:?}"
        );
        assert!(
            rows.iter().all(|row| row["process_machine_id"] == machine
                && row["transport"] == "ssh-mcp"
                && row["trace_id"].is_string()
                && row["origin_session_id"].is_string()),
            "trusted transport audit: {rows:?}"
        );
    }
    let requests = std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let admissions = requests
        .iter()
        .filter(|r| {
            r["method"] == "orbit/internal/drain/call" && r["params"]["name"] == "orbit.task.pull"
        })
        .collect::<Vec<_>>();
    assert_eq!(
        admissions[0]["params"]["arguments"]["request_id"],
        admissions[1]["params"]["arguments"]["request_id"]
    );
    assert!(
        !requests.iter().any(|r| r["method"] == "tools/list"),
        "internal schema preflight must not use public discovery"
    );
}
