//! A claimed before-PR reviewer's manifest read and report write from inside
//! a real agent sandbox, carried to a remote owner by its run's broker
//! (`docs/design/plugins/2_agent_call_broker.md` §3, §4.4;
//! `docs/runbooks/claimed-review-artifacts.md`).
//!
//! The owner is a separate Orbit home reached through an `ssh` stand-in that,
//! like the real client, needs `~/.ssh/known_hosts`; the agent sandbox masks
//! that directory. The claim comes from the owner's real probe, pull and bind
//! over that route; the follower's review ledger holds the admitted attempt
//! with its reviewer running. Inside the sandbox the built `orbit` serves the
//! reviewer through both the CLI and MCP, exactly as a provider calls it,
//! and SSH runs only in the unconfined broker. Last, the claim's reservation
//! elapses and the reviewer still reads and writes, until the owner recovers
//! the claim and a later pull supersedes it: while the follower's ledger still
//! records the reviewer running, the owner refuses the bridged calls.
//!
//! Linux confines the reviewer with Bubblewrap and macOS with `sandbox-exec`;
//! everything else is the same fixture.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::plugin::sandbox_mask::prepare_plugin_mask;
use orbit_core::test_support::{ReviewInvocationRecord, ReviewReserveRequest};
use orbit_engine::{PluginBrokerHandle, PluginBrokerRun, RuntimeHost};
use orbit_mcp::federated::{Destination, FederatedMcpHost, SshDestinationProbe};
use orbit_tools::plugin::BrokeredCaller;
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::task::TaskStatus;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    ActivityToolDenyPolicy, REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewBudget, ReviewConsumption, ReviewManifest, ReviewReservation,
    ReviewerInvocationEvent,
};
use serde_json::{Value, json};

const CHILD: &str = "ORBIT_CLAIMED_REVIEW_BRIDGE_FIXTURE";
/// The before-PR reviewer's catalog activity.
const REVIEWER: &str = "agent_review_repair";
/// The leaf run the claim is bound to; the reviewer runs in it.
const LEAF: &str = "leaf-review-1";
const LINEAGE: &str = "lineage-claimed-review";
/// Content of the follower's SSH trust file. It must never reach the sandbox.
const HOST_KEY_SECRET: &str = "owner-host ssh-ed25519 FIXTURE-HOST-KEY-SECRET";
/// Content of the follower's SSH identity. It must never reach the sandbox.
const PRIVATE_KEY_SECRET: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nFIXTURE-PRIVATE-KEY-SECRET\n";

#[test]
fn claimed_review_artifacts_cross_the_broker_from_a_confined_reviewer() {
    if let Some(reason) = sandbox_unavailable() {
        let _ = writeln!(
            std::io::stderr(),
            "skipped claimed-review bridge sandbox integration: {reason}"
        );
        return;
    }
    // Short and outside the per-user temp directory, which the sandbox may
    // replace with its own; canonical, because a macOS profile names real
    // paths and `/var` is a link there.
    let root = tempfile::Builder::new()
        .prefix("ocr")
        .tempdir_in("/var/tmp")
        .expect("sandbox-visible fixture root");
    let base = root.path().canonicalize().expect("resolve fixture root");
    let follower_home = base.join("follower/home");
    let path = std::env::join_paths(
        [base.join("bin"), follower_home.join("stub-bin")]
            .into_iter()
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            )),
    )
    .expect("PATH");
    let mut child = Command::new(std::env::current_exe().expect("test binary"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        child.env_remove(name);
    });
    let output = child
        .env(CHILD, &base)
        .env("HOME", &follower_home)
        .env("USERPROFILE", &follower_home)
        .env("PATH", path)
        .current_dir(&base)
        .args([
            "--ignored",
            "--exact",
            "claimed_review_bridge_sandbox::sandbox_fixture",
            "--nocapture",
        ])
        .output()
        .expect("isolated fixture");
    orbit_common::test_env::assert_child_test_passed(
        "claimed_review_bridge_sandbox::sandbox_fixture",
        output.status,
        &output.stdout,
        &output.stderr,
    );
}

#[cfg(target_os = "linux")]
fn sandbox_unavailable() -> Option<String> {
    let probe = orbit_exec::probe_bwrap();
    (!probe.available).then_some(probe.detail)
}

#[cfg(target_os = "macos")]
fn sandbox_unavailable() -> Option<String> {
    (!orbit_exec::sandbox_exec_available()).then(orbit_exec::sandbox_exec_unavailable_message)
}

/// One Orbit machine: its own home, global root and registered checkout.
struct Node {
    home: PathBuf,
    work: PathBuf,
}

impl Node {
    fn init(base: &Path, name: &str, prefix: &str) -> Self {
        let home = base.join(name).join("home");
        let work = base.join(name).join("work");
        let stub_bin = home.join("stub-bin");
        fs::create_dir_all(&stub_bin).expect("stub bin");
        fs::create_dir_all(&work).expect("work");
        // `orbit init` seeds crews from the agent CLIs on PATH.
        let codex = stub_bin.join("codex");
        fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("codex stub");
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).expect("executable");
        let git = Command::new("git")
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(&work)
            .status()
            .expect("git init");
        assert!(git.success());
        let node = Self { home, work };
        node.orbit(&[
            "init",
            "--non-interactive",
            "--skip-host-prerequisites",
            "--machine-name",
            name,
            "--task-prefix",
            prefix,
        ]);
        node.orbit(&["workspace", "init", "--name", name]);
        node
    }

    fn global(&self) -> PathBuf {
        self.home.join(".orbit")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
        orbit_common::test_env::clear_inherited_authority(|name| {
            command.env_remove(name);
        });
        let path = std::env::join_paths(std::iter::once(self.home.join("stub-bin")).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .expect("PATH");
        command
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("PATH", path);
        command
    }

    fn orbit(&self, args: &[&str]) -> Value {
        let output = self.command().args(args).output().expect("orbit");
        assert!(
            output.status.success(),
            "orbit {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
    }

    fn tool(&self, name: &str, input: Value) -> Value {
        self.orbit(&["tool", "run", name, "--input", &input.to_string()])
    }

    fn runtime(&self) -> OrbitRuntime {
        OrbitRuntime::from_roots(&self.global(), &self.work.join(".orbit")).expect("runtime")
    }

    fn machine_id(&self) -> String {
        orbit_mcp::mcp_server_identity(&self.global(), None, orbit_mcp::McpSessionAuthority::Agent)
            .expect("machine identity")
            .process_machine_id
    }
}

/// An `ssh` stand-in for the follower: it needs the caller's
/// `~/.ssh/known_hosts`, as the real client does to verify the owner, then
/// runs the runtime-composed remote argv against the owner's home. A marker
/// file drops the answer to the next artifact put after the owner commits it.
fn plant_ssh(base: &Path, owner: &Node) -> PathBuf {
    let bin = base.join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let lose = base.join("lose-next-put-answer");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, os, shlex, subprocess, sys, threading
known = os.path.join(os.environ.get('HOME', ''), '.ssh', 'known_hosts')
try:
    with open(known) as trust:
        trust.read()
except OSError as error:
    sys.stderr.write('hostkeys_find_by_key_hostfile: %s: %s\r\nHost key verification failed.\r\n' % (known, error.strerror))
    sys.exit(255)
argv = shlex.split(sys.argv[-1])
assert argv.pop(0) == 'orbit'
env = os.environ.copy()
env['HOME'] = {home}
env['USERPROFILE'] = {home}
child = subprocess.Popen([{binary}] + argv, cwd={work}, env=env,
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
puts = set()
def forward():
    for line in sys.stdin.buffer:
        message = json.loads(line)
        name = str(message.get('params', {{}}).get('name', ''))
        if message.get('method') == 'tools/call' and 'artifact' in name and 'put' in name:
            puts.add(message.get('id'))
        child.stdin.write(line)
        child.stdin.flush()
    child.stdin.close()
threading.Thread(target=forward, daemon=True).start()
for line in child.stdout:
    message = json.loads(line)
    if message.get('id') in puts and os.path.exists({lose}):
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
        lose = json!(lose),
    );
    fs::write(bin.join("ssh"), script).expect("ssh stub");
    fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755)).expect("executable");
    lose
}

/// The broker's view of the reviewer run, as the step runner builds it.
fn broker_run(
    run_id: &str,
    activity: &str,
    task: &str,
    workspace: &str,
    caller: &BrokeredCaller,
) -> PluginBrokerRun {
    PluginBrokerRun {
        run_id: run_id.to_string(),
        job_run_id: Some(LEAF.to_string()),
        task_id: Some(task.to_string()),
        activity_name: activity.to_string(),
        agent_name: Some("codex".to_string()),
        model_name: None,
        workspace: Some(workspace.to_string()),
        allowed_tools: Vec::new(),
        tool_deny_policy: Some(ActivityToolDenyPolicy {
            activity: activity.to_string(),
            disallow_list: vec!["orbit.agent.invoke".to_string()],
        }),
        caller: caller.clone(),
    }
}

#[test]
#[ignore = "isolated child of the real-sandbox claimed-review bridge test"]
fn sandbox_fixture() {
    let Some(base) = std::env::var_os(CHILD) else {
        return;
    };
    let base = PathBuf::from(base);
    let owner = Node::init(&base, "owner", "TSO");
    let follower = Node::init(&base, "follower", "TSF");
    assert_eq!(
        Some(follower.home.as_os_str()),
        std::env::var_os("HOME").as_deref(),
        "the follower is this process's machine"
    );
    let lose = plant_ssh(&base, &owner);
    // The follower's SSH trust, readable here and masked in the sandbox.
    fs::create_dir_all(follower.home.join(".ssh")).expect("ssh dir");
    fs::write(follower.home.join(".ssh/known_hosts"), HOST_KEY_SECRET).expect("known_hosts");
    let identity = follower.home.join(".ssh/id_ed25519");
    fs::write(&identity, PRIVATE_KEY_SECRET).expect("identity");
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).expect("protect identity");

    // A pullable owner task.
    fs::write(owner.work.join("work.rs"), "fn work() {}\n").expect("context file");
    let task = owner.tool(
        "orbit.task.add",
        json!({"title": "Claimed review fixture", "description": "Disposable claim",
               "complexity": "low", "model": "codex", "context_files": ["file:work.rs"]}),
    )["id"]
        .as_str()
        .expect("task id")
        .to_string();
    owner.tool(
        "orbit.task.update",
        json!({"id": task, "plan": "Implement work.rs and validate"}),
    );
    owner.tool(
        "orbit.task.update",
        json!({"id": task, "status": "backlog"}),
    );
    let owner_machine = owner.machine_id();
    let owner_workspace = owner.runtime().workspace_id().expect("owner workspace");
    let selector = format!("{owner_machine}/{owner_workspace}");
    let follower_machine = follower.machine_id();

    // The claim, over the same SSH route a follower drain uses.
    let route = || {
        FederatedMcpHost::new(
            vec![Destination::ssh("isolated-owner", &owner_machine)],
            Arc::new(SshDestinationProbe::new(
                follower_machine.clone(),
                Duration::from_secs(60),
                Duration::from_secs(60),
                None,
                orbit_mcp::McpSessionAuthority::Agent,
            )),
        )
    };
    let drain = |name: &str, mut input: Value| {
        input["workspace"] = json!(selector);
        route()
            .call_internal_drain(name, input, ToolSessionContext::default())
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    };
    let probe = drain("orbit.drain.probe", json!({}));
    let admitted = drain(
        "orbit.task.pull",
        json!({"request_id": "claimed-review-admission",
               "caller_version": probe["binary_version"],
               "caller_schema": probe["protocol_schema"], "caller_before_pr": false,
               "run_context": {"run_id": "follower-drain", "job_name": "workspace_pull_pipeline"},
               "ship": probe["ship"]}),
    );
    let claim = admitted["receipt"]["claim"].clone();
    assert_eq!(claim["task_id"], task.as_str(), "{admitted}");
    let bound = drain(
        "orbit.drain.claim.bind",
        json!({"claim_id": claim["claim_id"], "run_id": LEAF, "ship": probe["ship"]}),
    );
    assert_eq!(bound["phase"], "running", "{bound}");
    let binding: WorkerInvocation = serde_json::from_value(json!({
        "owner_machine_id": owner_machine,
        "owner_workspace_id": owner_workspace,
        "owner_destination": selector,
        "task_id": task,
        "claim_id": claim["claim_id"],
        "execution": claim["executed_on"],
        "bound_run_id": LEAF,
    }))
    .expect("worker binding");

    // The leaf's step runner: the follower runtime bound to the claim, with
    // the owner route its composition installs.
    let runner = follower
        .runtime()
        .with_automation_machine_identity(Some(follower_machine.clone()))
        .with_worker_invocation(binding, Arc::new(route()))
        .expect("bind the leaf");
    let workspace = runner.workspace_id().expect("follower workspace");

    // The admitted attempt, its reviewer running in the leaf.
    let store = runner.review_store().expect("review store");
    let candidate = SourceRevision {
        commit: "a".repeat(40),
        tree: "b".repeat(40),
    };
    let (reservation, _) = store
        .review_reserve(
            &workspace,
            &ReviewReserveRequest {
                lineage_key: LINEAGE,
                task_ids: std::slice::from_ref(&task),
                run_id: LEAF,
                task_meaning_digest: "digest",
                candidate: &candidate,
                budget: ReviewBudget::default(),
                now: Utc::now(),
            },
        )
        .expect("reserve");
    let ReviewReservation::Reserved { attempt } = reservation else {
        panic!("a fresh lineage reserves an attempt: {reservation:?}");
    };
    let attempt_id = attempt.attempt_id.clone();
    store
        .review_record_invocation(
            &workspace,
            &ReviewInvocationRecord {
                lineage_key: LINEAGE,
                attempt_id: &attempt_id,
                run_id: LEAF,
                event: ReviewerInvocationEvent::Started {
                    timeout_seconds: 1800,
                },
                now: Utc::now(),
            },
        )
        .expect("reviewer started");

    // The manifest the gate pins on the owner, through the claim, first for
    // another attempt: the reviewer must refuse it as stale.
    let scratch = follower.work.join(".orbit/tmp");
    fs::create_dir_all(&scratch).expect("scratch");
    let pin_manifest = |attempt: &str| -> Vec<u8> {
        let bytes = serde_json::to_vec_pretty(&manifest(&task, attempt, &candidate)).unwrap();
        let source = scratch.join(REVIEW_MANIFEST_ARTIFACT);
        fs::write(&source, &bytes).expect("manifest source");
        runner
            .run_tool(
                "orbit.task.artifact.put",
                json!({"id": task, "path": REVIEW_MANIFEST_ARTIFACT, "source_path": source}),
            )
            .expect("the gate pins the manifest on the owner");
        bytes
    };
    pin_manifest("attempt-from-an-earlier-run");

    // The reviewer's sources: its report, the one it first attaches over
    // MCP, one for another attempt, an oversize file and a link out of the
    // workspace.
    let report = serde_json::to_vec(&json!({
        "schema_version": REVIEW_CONTRACT_VERSION, "attempt_id": attempt_id,
        "verdict": "incomplete", "summary": "Bridge fixture review.",
        "findings": [], "validation": [], "escalation": "fixture only"
    }))
    .unwrap();
    let report_source = scratch.join("report.json");
    fs::write(&report_source, &report).expect("report");
    let mut mcp_report = serde_json::from_slice::<Value>(&report).unwrap();
    mcp_report["summary"] = json!("Bridge fixture review, attached over MCP.");
    let mcp_report = serde_json::to_vec(&mcp_report).unwrap();
    fs::write(scratch.join("mcp-report.json"), &mcp_report).expect("MCP report");
    // A subdirectory an agent may call from.
    let nested = follower.work.join("src/nested");
    fs::create_dir_all(&nested).expect("nested cwd");
    let mut stale = serde_json::from_slice::<Value>(&report).unwrap();
    stale["attempt_id"] = json!("attempt-from-an-earlier-run");
    fs::write(scratch.join("stale.json"), stale.to_string()).expect("stale report");
    fs::write(scratch.join("oversize.json"), vec![b' '; 1024 * 1024 + 1]).expect("oversize");
    // A link out of the workspace, to bytes a valid report would carry.
    let outside = base.join("outside-report.json");
    fs::write(&outside, &report).expect("outside source");
    std::os::unix::fs::symlink(&outside, scratch.join("link.json")).expect("symlink");

    let profile = ResolvedFsProfile {
        name: "claimed_review_probe".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![format!("{}/**", base.display())],
    };
    let caller = BrokeredCaller {
        worktree: follower.work.clone(),
        fs_profile: profile.clone(),
        proc_allowed_programs: Vec::new(),
        proc_disallowed_programs: None,
    };
    let provider = base.join("provider.py");
    fs::write(&provider, PROVIDER).expect("provider");

    // Phase 1: the running reviewer reads, writes, retries and is refused.
    let broker = runner
        .start_plugin_broker(&broker_run(LEAF, REVIEWER, &task, &workspace, &caller))
        .expect("start broker")
        .expect("Unix broker");
    // The first read finds the stale manifest; the provider waits for the
    // pinned one before going on.
    let pinned_signal = base.join("manifest-pinned");
    let stale_seen = base.join("stale-manifest-seen");
    let mcp_put_done = base.join("mcp-put-done");
    let mcp_put_checked = base.join("mcp-put-checked");
    let first = Confined::spawn(
        &runner,
        &follower,
        &profile,
        &provider,
        &[
            "running",
            &task,
            scratch.to_str().unwrap(),
            lose.to_str().unwrap(),
            stale_seen.to_str().unwrap(),
            pinned_signal.to_str().unwrap(),
            &selector,
            nested.to_str().unwrap(),
            mcp_put_done.to_str().unwrap(),
            mcp_put_checked.to_str().unwrap(),
        ],
        &[broker.as_ref()],
        &[],
    );
    wait_for(&stale_seen, Duration::from_secs(180));
    let manifest_bytes = pin_manifest(&attempt_id);
    fs::write(&pinned_signal, "").expect("signal the pinned manifest");
    // The MCP put alone attached the owner's report, before any CLI put.
    wait_for(&mcp_put_done, Duration::from_secs(300));
    let owner_runtime = owner.runtime();
    let attached_over_mcp = owner_runtime
        .get_task_artifact(&task, REVIEW_REPORT_ARTIFACT)
        .expect("owner artifact")
        .map(|artifact| artifact.content);
    fs::write(&mcp_put_checked, "").expect("signal the checked MCP put");
    let running = first.finish();
    assert_eq!(
        attached_over_mcp.as_deref(),
        Some(mcp_report.as_slice()),
        "the MCP put attached its exact bytes on the owner: {running}"
    );
    assert!(
        running["mcp_put"].get("error").is_none() && running["mcp_put"]["id"] == task.as_str(),
        "the MCP put answered success for the task: {running}"
    );
    drop(broker);

    let text = running.to_string();
    assert!(
        !text.contains("FIXTURE-HOST-KEY-SECRET") && !text.contains("FIXTURE-PRIVATE-KEY-SECRET"),
        "no credential reaches the reviewer: {running}"
    );
    for credential in ["known_hosts_readable", "private_key_readable"] {
        assert_eq!(
            running[credential], false,
            "the sandbox masks ~/.ssh ({credential}): {running}"
        );
    }
    assert_eq!(running["direct_ssh"]["code"], 255, "{running}");
    let cause = running["direct_ssh"]["stderr"].as_str().unwrap();
    assert!(
        cause.contains("Host key verification failed"),
        "SSH inside the sandbox cannot verify the owner: {running}"
    );
    refused(&running["stale_manifest"], "review_manifest_stale");
    for read in [
        &running["cli_get"]["output"],
        &running["mcp_get"],
        &running["nested_cli_get"]["output"],
        &running["nested_mcp_get"],
    ] {
        assert_eq!(
            read["content"].as_str().map(str::as_bytes),
            Some(manifest_bytes.as_slice()),
            "CLI and MCP read the pinned manifest's exact bytes, from the workspace root or a \
         subdirectory naming the owner: {running}"
        );
    }
    for write in ["cli_put", "cli_put_replay", "retry_put", "nested_cli_put"] {
        assert_eq!(running[write]["code"], 0, "{write}: {running}");
    }
    assert_ne!(
        running["lost_put"]["code"], 0,
        "a put whose answer was lost does not report success: {running}"
    );
    assert!(
        PathBuf::from(format!("{}.consumed", lose.display())).exists(),
        "the owner committed a put whose answer was then lost"
    );
    refused(&running["wrong_path"], "claimed_review_bridge_refused");
    refused(&running["cross_task"], "claimed_review_bridge_refused");
    refused(&running["stale_report"], "claimed_review_bridge_refused");
    refused(&running["symlink"], "");
    refused(&running["oversize"], "");
    // The same refusals over MCP, whose adapter prepares the input itself.
    let mcp = &running["mcp_refusals"];
    mcp_refused(&mcp["wrong_path"], "claimed_review_bridge_refused");
    mcp_refused(&mcp["cross_task"], "claimed_review_bridge_refused");
    mcp_refused(&mcp["stale_report"], "claimed_review_bridge_refused");
    mcp_refused(&mcp["symlink"], "");
    mcp_refused(&mcp["oversize"], "");
    mcp_refused(&mcp["unrelated"], "");
    mcp_refused(&mcp["other_owner"], "");
    assert_ne!(
        running["unrelated"]["code"], 0,
        "no other tool is forwarded; its own route still needs SSH: {running}"
    );
    for (forged, response) in running["forged"].as_object().unwrap() {
        assert_eq!(
            response["ok"], false,
            "{forged}: a forged broker request is refused: {running}"
        );
    }

    // The owner holds the report's exact bytes once, and no certificate.
    let attached = owner_runtime
        .get_task_artifact(&task, REVIEW_REPORT_ARTIFACT)
        .expect("owner artifact")
        .expect("report attached");
    assert_eq!(attached.content, report, "the owner stores the exact bytes");
    let paths = owner_runtime
        .get_task_artifacts(&task)
        .expect("owner artifacts")
        .into_iter()
        .map(|artifact| artifact.path)
        .collect::<Vec<_>>();
    assert!(
        paths.iter().all(|path| path != "review-certificate.json")
            && store
                .review_certificate(&workspace, &attempt_id)
                .expect("certificate read")
                .is_none(),
        "the bridge never writes a certificate: {paths:?}"
    );

    // Each bridged call is one brokered row naming the reviewer's task and
    // activity on the follower, and one claim-scoped row on the owner.
    let follower_rows = runner
        .list_audit_events(
            None,
            Some("orbit.task.artifact.put".to_string()),
            None,
            None,
            100,
        )
        .expect("follower audit");
    let brokered = follower_rows
        .iter()
        .filter(|row| row.brokered)
        .collect::<Vec<_>>();
    assert!(
        brokered.iter().all(|row| row.peer_pid.is_some()
            && row.task_id.as_deref() == Some(task.as_str())
            && row.activity_id.as_deref() == Some(REVIEWER)),
        "{brokered:?}"
    );
    assert!(
        brokered.len() >= 5,
        "every bridged put is audited: {follower_rows:?}"
    );
    assert!(
        follower_rows
            .iter()
            .any(|row| !row.brokered
                && row.status != orbit_types::telemetry::AuditEventStatus::Success),
        "a put refused before the broker is audited where it was refused: {follower_rows:?}"
    );
    let owner_rows = owner_runtime
        .list_audit_events(
            None,
            Some("orbit.task.artifact.put".to_string()),
            None,
            None,
            100,
        )
        .expect("owner audit");
    let from_follower = owner_rows
        .iter()
        .filter(|row| row.caller_machine_id.as_deref() == Some(follower_machine.as_str()))
        .collect::<Vec<_>>();
    assert!(
        !from_follower.is_empty()
            && from_follower
                .iter()
                .all(|row| row.transport == Some(orbit_types::tool::McpTransport::SshMcp)),
        "{owner_rows:?}"
    );

    // Phase 2: the reviewer has finished, so its attempt no longer admits
    // calls; another activity's broker never did; and a broker that has
    // shut down is named as the cause.
    store
        .review_record_invocation(
            &workspace,
            &ReviewInvocationRecord {
                lineage_key: LINEAGE,
                attempt_id: &attempt_id,
                run_id: LEAF,
                event: ReviewerInvocationEvent::Finished { runtime_seconds: 5 },
                now: Utc::now(),
            },
        )
        .expect("reviewer finished");
    let stopped = runner
        .start_plugin_broker(&broker_run(
            "leaf-review-stopped",
            REVIEWER,
            &task,
            &workspace,
            &caller,
        ))
        .expect("start broker")
        .expect("Unix broker");
    let dead_socket = stopped.socket_path().to_path_buf();
    drop(stopped);
    let finished = runner
        .start_plugin_broker(&broker_run(
            "leaf-review-finished",
            REVIEWER,
            &task,
            &workspace,
            &caller,
        ))
        .expect("start broker")
        .expect("Unix broker");
    let implementer = runner
        .start_plugin_broker(&broker_run(
            "leaf-implement",
            "agent_implement",
            &task,
            &workspace,
            &caller,
        ))
        .expect("start broker")
        .expect("Unix broker");
    let after = Confined::spawn(
        &runner,
        &follower,
        &profile,
        &provider,
        &["finished", &task],
        &[finished.as_ref(), implementer.as_ref()],
        &[
            (
                "ORBIT_TEST_IMPLEMENTER_BROKER",
                implementer.socket_path().display().to_string(),
            ),
            ("ORBIT_TEST_DEAD_BROKER", dead_socket.display().to_string()),
        ],
    )
    .finish();
    refused(&after["finished"], "review_attempt_stale");
    refused(&after["other_activity"], "claimed_review_bridge_refused");
    refused(
        &after["broker_gone"],
        "could not reach this run's coordinator",
    );
    mcp_refused(
        &after["broker_gone_mcp"],
        "could not reach this run's coordinator",
    );
    let unavailable_rows = runner
        .list_audit_events(
            None,
            Some("orbit.task.artifact.get".to_string()),
            None,
            None,
            100,
        )
        .expect("follower failure audit");
    let caller_failures = unavailable_rows
        .iter()
        .filter(|row| {
            !row.brokered
                && row.status != orbit_types::telemetry::AuditEventStatus::Success
                && row.error_message.as_deref().is_some_and(|message| {
                    message.contains("could not reach this run's coordinator")
                })
        })
        .count();
    assert_eq!(
        caller_failures, 2,
        "the CLI and MCP each retain one caller-side failure when the broker is stopped: {unavailable_rows:?}"
    );
    assert_eq!(
        owner_runtime
            .get_task_artifact(&task, REVIEW_REPORT_ARTIFACT)
            .expect("owner artifact")
            .expect("report attached")
            .content,
        report,
        "refused calls change nothing on the owner"
    );

    // Phase 3: the reviewer runs again by the follower's ledger and the
    // claim's reservation window really closes. An elapsed reservation ends
    // nothing, so the live claim still reads and writes; then the owner
    // recovers the claim and a later pull supersedes it, and the owner
    // refuses the bridged read and write as stale with nothing changed.
    store
        .review_record_invocation(
            &workspace,
            &ReviewInvocationRecord {
                lineage_key: LINEAGE,
                attempt_id: &attempt_id,
                run_id: LEAF,
                event: ReviewerInvocationEvent::Started {
                    timeout_seconds: 1800,
                },
                now: Utc::now(),
            },
        )
        .expect("reviewer restarted");
    let bridged = |run_id: &str| {
        let broker = runner
            .start_plugin_broker(&broker_run(run_id, REVIEWER, &task, &workspace, &caller))
            .expect("start broker")
            .expect("Unix broker");
        Confined::spawn(
            &runner,
            &follower,
            &profile,
            &provider,
            &["calls", &task, scratch.to_str().unwrap()],
            &[broker.as_ref()],
            &[],
        )
        .finish()
    };
    let elapsed = elapse_reservation(
        &owner_runtime,
        claim["claim_id"].as_str().expect("claim id"),
    );
    assert_eq!(elapsed["phase"], "running", "{elapsed}");
    let live = bridged("leaf-review-elapsed");
    for read in [&live["cli_get"]["output"], &live["mcp_get"]] {
        assert_eq!(
            read["content"].as_str().map(str::as_bytes),
            Some(manifest_bytes.as_slice()),
            "an elapsed reservation leaves the claim reading: {live}"
        );
    }
    assert_eq!(live["cli_put"]["code"], 0, "{live}");
    assert!(
        live["mcp_put"].get("error").is_none() && live["mcp_put"]["id"] == task.as_str(),
        "an elapsed reservation leaves the claim writing: {live}"
    );
    owner_runtime
        .recover_claim_as_operator(
            claim["claim_id"].as_str().expect("claim id"),
            "running",
            TaskStatus::Backlog,
            "operator",
            "The claim outlived its reservation.",
            "claimed-review-revocation",
        )
        .expect("the owner revokes the claim");
    let owner_evidence = || {
        let artifacts = owner_runtime
            .get_task_artifacts(&task)
            .expect("owner artifacts")
            .into_iter()
            .map(|artifact| (artifact.path, artifact.content))
            .collect::<Vec<_>>();
        let certificate = owner_runtime
            .review_store()
            .expect("owner review store")
            .review_certificate(&owner_workspace, &attempt_id)
            .expect("owner certificate read");
        (artifacts, certificate.is_some())
    };
    let before = owner_evidence();
    let stale = |run_id: &str| {
        let report = bridged(run_id);
        refused(&report["cli_get"], "stale_claim");
        refused(&report["cli_put"], "stale_claim");
        mcp_refused(&report["mcp_get"], "stale_claim");
        mcp_refused(&report["mcp_put"], "stale_claim");
    };
    stale("leaf-review-revoked");
    let readmitted = drain(
        "orbit.task.pull",
        json!({"request_id": "claimed-review-readmission",
               "caller_version": probe["binary_version"],
               "caller_schema": probe["protocol_schema"], "caller_before_pr": false,
               "run_context": {"run_id": "follower-drain-2", "job_name": "workspace_pull_pipeline"},
               "ship": probe["ship"]}),
    );
    let next = readmitted["receipt"]["claim"].clone();
    assert_eq!(next["task_id"], task.as_str(), "{readmitted}");
    assert_ne!(next["claim_id"], claim["claim_id"], "{readmitted}");
    let rebound = drain(
        "orbit.drain.claim.bind",
        json!({"claim_id": next["claim_id"], "run_id": "leaf-review-2", "ship": probe["ship"]}),
    );
    assert_eq!(rebound["phase"], "running", "{rebound}");
    stale("leaf-review-superseded");
    assert_eq!(
        owner_evidence(),
        before,
        "stale-claim refusals change nothing on the owner"
    );
    assert!(!before.1, "the bridge never records a certificate");
}

/// Let the owner's reservation for `claim_id` run out: its window is moved,
/// in the owner's own store, to one that closed a minute ago. Returns the
/// claim as the owner's console, read at the real clock, then reports it.
fn elapse_reservation(owner: &OrbitRuntime, claim_id: &str) -> Value {
    let console = || {
        owner.distributed_claim_console().expect("owner console")["claims"]
            .as_array()
            .expect("claims")
            .iter()
            .find(|claim| claim["claim_id"] == claim_id)
            .cloned()
            .expect("the claim")
    };
    let current = console();
    let reservation_id = current["reservation"]["id"].as_str().expect("reservation");
    let window = current["reservation"]["expires_at"]
        .as_str()
        .expect("window");
    let expires_at = Utc::now() - chrono::Duration::minutes(1);
    let shift = chrono::DateTime::parse_from_rfc3339(window)
        .expect("window time")
        .signed_duration_since(expires_at);
    let expired = expires_at.to_rfc3339();
    let workspace_id = owner.workspace_id().expect("owner workspace");
    let connection =
        rusqlite::Connection::open(owner.global_root().join("orbit.db")).expect("owner store");
    let created: String = connection
        .query_row(
            "SELECT created_at FROM task_reservations WHERE reservation_id=?1",
            rusqlite::params![reservation_id],
            |row| row.get(0),
        )
        .expect("the claim's reservation row");
    let created = (chrono::DateTime::parse_from_rfc3339(&created).expect("created time") - shift)
        .to_rfc3339();
    let moved = connection
        .execute(
            "UPDATE task_reservations SET created_at=?1, expires_at=?2 WHERE reservation_id=?3",
            rusqlite::params![created, expired, reservation_id],
        )
        .expect("move the reservation");
    assert_eq!(moved, 1, "the claim's reservation row");
    // The claim and its admission keep their own copy of the window.
    let copies = connection
        .execute(
            "UPDATE task_coordination_rows SET payload_json=replace(payload_json, ?1, ?2)
             WHERE workspace_id=?3 AND instr(payload_json, ?1) > 0",
            rusqlite::params![window, expired, workspace_id],
        )
        .expect("move the claim's window");
    assert!(copies >= 1, "the claim records its reservation window");
    let elapsed = console();
    assert_eq!(elapsed["reservation"]["expires_at"], expired.as_str());
    assert_eq!(elapsed["reservation"]["expired"], true, "{elapsed}");
    elapsed
}

/// A refused MCP call: an error answer that names `cause`.
fn mcp_refused(result: &Value, cause: &str) {
    let error = result
        .get("error")
        .unwrap_or_else(|| panic!("refused: {result}"));
    assert!(
        error.to_string().contains(cause),
        "refusal names `{cause}`: {result}"
    );
}

/// A refused call: a failed exit whose error names `cause`.
fn refused(result: &Value, cause: &str) {
    assert_ne!(result["code"], 0, "refused: {result}");
    assert!(
        result["stderr"]
            .as_str()
            .unwrap_or_default()
            .contains(cause),
        "refusal names `{cause}`: {result}"
    );
}

fn wait_for(path: &Path, limit: Duration) {
    let deadline = std::time::Instant::now() + limit;
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn manifest(task: &str, attempt_id: &str, candidate: &SourceRevision) -> ReviewManifest {
    ReviewManifest {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.to_string(),
        lineage_key: LINEAGE.to_string(),
        task_ids: vec![task.to_string()],
        task_digests: BTreeMap::from([(task.to_string(), "digest".to_string())]),
        required_validation_commands: Some(vec!["make ci-fast".to_string()]),
        task_meaning_digest: "digest".to_string(),
        repository: "owner/repository".to_string(),
        base: SourceRevision {
            commit: "c".repeat(40),
            tree: "d".repeat(40),
        },
        candidate: candidate.clone(),
        implementation_commits: Vec::new(),
        implementer_summaries: BTreeMap::new(),
        reviewer_crew: "sol".to_string(),
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: 1,
        budget: ReviewBudget::default(),
        remaining: ReviewConsumption { seconds: 1800 },
        issued_at: Utc::now(),
    }
}

/// The reviewer provider, confined as a managed launch confines it.
struct Confined {
    child: std::process::Child,
    #[cfg(target_os = "macos")]
    _profile: Arc<tempfile::NamedTempFile>,
}

impl Confined {
    #[allow(clippy::too_many_arguments)]
    fn spawn(
        runner: &OrbitRuntime,
        follower: &Node,
        profile: &ResolvedFsProfile,
        provider: &Path,
        args: &[&str],
        brokers: &[&dyn PluginBrokerHandle],
        extra_env: &[(&str, String)],
    ) -> Self {
        let mut argv = vec![
            provider.display().to_string(),
            env!("CARGO_BIN_EXE_orbit").to_string(),
        ];
        argv.extend(args.iter().map(ToString::to_string));
        let mut env = runner.agent_subprocess_environment(&[]);
        env.push((
            "ORBIT_PLUGIN_BROKER".to_string(),
            brokers[0].socket_path().display().to_string(),
        ));
        env.push(("ORBIT_WORKER_CONTEXT_REQUIRED".to_string(), "1".to_string()));
        env.extend(
            extra_env
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone())),
        );
        let prepared = prepare_plugin_mask(&follower.global()).expect("plugin mask");
        let confined = Self::launch(profile, &argv, &env, &follower.work, prepared);
        let pid = confined.child.id();
        // What the step runner records for the provider it just spawned.
        RuntimeHost::register_worker_process(runner, pid).expect("record the worker");
        RuntimeHost::register_worker_pid_namespace(runner, pid).expect("record the namespace");
        for broker in brokers {
            broker.bind_sandbox(pid).expect("authenticate the sandbox");
        }
        confined
    }

    #[cfg(target_os = "linux")]
    fn launch(
        profile: &ResolvedFsProfile,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        prepared: orbit_core::runtime::plugin::sandbox_mask::PreparedPluginMask,
    ) -> Self {
        let mask = orbit_exec::LinuxBwrapMask {
            sentinel: prepared.sentinel,
            targets: prepared.trees,
        };
        let plan = orbit_exec::compile_linux_bwrap_argv_with_authority(
            profile,
            "/usr/bin/python3",
            argv,
            Some(cwd),
            false,
            Vec::new(),
            Some(&mask),
        )
        .expect("compile the agent sandbox");
        let child = orbit_exec::spawn_under_linux_bwrap(orbit_exec::LinuxBwrapSpawnRequest {
            plan: &plan,
            env,
            cwd: Some(cwd),
            stdin: Stdio::null(),
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
        })
        .expect("spawn real sandbox");
        Self { child }
    }

    #[cfg(target_os = "macos")]
    fn launch(
        profile: &ResolvedFsProfile,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        prepared: orbit_core::runtime::plugin::sandbox_mask::PreparedPluginMask,
    ) -> Self {
        let mut profile_text =
            orbit_exec::compile_macos_sandbox_profile(profile, "codex").expect("compile profile");
        orbit_exec::append_macos_subpath_mask(&mut profile_text, &prepared.trees);
        let (child, profile_file) =
            orbit_exec::spawn_under_macos_sandbox(orbit_exec::MacosSandboxSpawnRequest {
                profile_text: &profile_text,
                program: "/usr/bin/python3",
                args: argv,
                env,
                cwd: Some(cwd),
                stdin: Stdio::null(),
                stdout: Stdio::piped(),
                stderr: Stdio::piped(),
                inherited_fds: &[],
            })
            .expect("spawn real sandbox");
        Self {
            child,
            _profile: profile_file,
        }
    }

    /// Wait for the provider and return its report.
    fn finish(self) -> Value {
        let output = orbit_exec::supervise_child(self.child, Some(600_000), None)
            .expect("supervise sandbox")
            .result;
        assert!(
            output.success,
            "sandbox provider: {}\n{}",
            output.stdout, output.stderr
        );
        serde_json::from_str(&output.stdout)
            .unwrap_or_else(|error| panic!("provider report: {error}\n{}", output.stdout))
    }
}

/// The reviewer's side: the calls an agent makes through the built `orbit`,
/// plus raw broker requests no `orbit` would send.
const PROVIDER: &str = r#"import json, os, socket, struct, subprocess, sys, time
orbit, mode, task = sys.argv[1], sys.argv[2], sys.argv[3]
MANIFEST, REPORT = 'review-manifest.json', 'review-report.json'

def run(argv, env=None, cwd=None):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=300, env=env, cwd=cwd)
    report = {'code': result.returncode, 'stderr': result.stderr[-4000:]}
    if result.returncode == 0:
        try:
            report['output'] = json.loads(result.stdout)
        except ValueError:
            report['stdout'] = result.stdout[-4000:]
    return report

def tool(name, payload, env=None, cwd=None):
    return run([orbit, 'tool', 'run', name, '--input', json.dumps(payload)], env, cwd)

def with_broker(socket_path):
    env = dict(os.environ)
    env['ORBIT_PLUGIN_BROKER'] = socket_path
    return env

def mcp(calls, cwd=None, env=None):
    server = subprocess.Popen([orbit, 'mcp', 'serve'], stdin=subprocess.PIPE,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, cwd=cwd, env=env)
    def send(message):
        server.stdin.write(json.dumps(message) + '\n')
        server.stdin.flush()
    def answer(ident):
        while True:
            line = server.stdout.readline()
            if not line:
                return {'error': server.stderr.read()[-4000:]}
            message = json.loads(line)
            if message.get('id') == ident:
                return message
    send({'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {
        'protocolVersion': '2025-06-18', 'capabilities': {},
        'clientInfo': {'name': 'reviewer', 'version': '0'}}})
    answer(0)
    send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
    results = []
    for ident, (name, arguments) in enumerate(calls, 1):
        send({'jsonrpc': '2.0', 'id': ident, 'method': 'tools/call',
              'params': {'name': name, 'arguments': arguments}})
        response = answer(ident)
        result = response.get('result', {})
        if result.get('isError') or 'error' in response:
            results.append({'error': response})
        else:
            results.append(result.get('structuredContent'))
    server.stdin.close()
    server.wait(timeout=60)
    return results

def raw(tool_name, payload):
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.connect(os.environ['ORBIT_PLUGIN_BROKER'])
    body = json.dumps({'schema_version': 1, 'tool': tool_name, 'input': payload,
        'cwd': os.getcwd(), 'workspace': None, 'entry_point': 'cli',
        'dry_run': False}).encode()
    connection.sendall(struct.pack('>I', len(body)) + body)
    def exactly(count):
        data = b''
        while len(data) < count:
            chunk = connection.recv(count - len(data))
            if not chunk:
                raise EOFError('broker closed')
            data += chunk
        return data
    length = struct.unpack('>I', exactly(4))[0]
    return json.loads(exactly(length))

report = {}
if mode == 'running':
    scratch, lose, stale_seen, pinned, selector, nested, mcp_done, mcp_checked = sys.argv[4:12]
    def wait(path):
        deadline = time.time() + 180
        while not os.path.exists(path) and time.time() < deadline:
            time.sleep(0.1)
    def readable(path):
        try:
            with open(path) as handle:
                handle.read()
            return True
        except OSError:
            return False
    report['known_hosts_readable'] = readable(os.path.expanduser('~/.ssh/known_hosts'))
    report['private_key_readable'] = readable(os.path.expanduser('~/.ssh/id_ed25519'))
    report['direct_ssh'] = run(['ssh', '-T', '--', 'isolated-owner', 'orbit mcp serve'])
    get = {'id': task, 'path': MANIFEST}
    report['stale_manifest'] = tool('orbit.task.artifact.get', get)
    open(stale_seen, 'w').close()
    wait(pinned)
    report['cli_get'] = tool('orbit.task.artifact.get', get)
    put = {'id': task, 'path': REPORT, 'source_path': os.path.join(scratch, 'report.json')}
    mcp_put = dict(put, source_path=os.path.join(scratch, 'mcp-report.json'))
    report['mcp_get'], report['mcp_put'] = mcp([('orbit_task_artifact_get', get),
                                                ('orbit_task_artifact_put', mcp_put)])
    # The host checks the owner's bytes before any CLI put replaces them.
    open(mcp_done, 'w').close()
    wait(mcp_checked)
    source = lambda name: dict(put, source_path=os.path.join(scratch, name + '.json'))
    names = ['wrong_path', 'cross_task', 'stale_report', 'symlink', 'oversize', 'unrelated',
             'other_owner']
    report['mcp_refusals'] = dict(zip(names, mcp([
        ('orbit_task_artifact_get', {'id': task, 'path': REPORT}),
        ('orbit_task_artifact_get', {'id': 'TSO-999', 'path': MANIFEST}),
        ('orbit_task_artifact_put', source('stale')),
        ('orbit_task_artifact_put', source('link')),
        ('orbit_task_artifact_put', source('oversize')),
        ('orbit_task_show', {'id': task}),
        ('orbit_task_artifact_get', dict(get, workspace='another-owner/ws_other')),
    ])))
    # From a subdirectory, naming the owner's selector explicitly.
    named = dict(get, workspace=selector)
    report['nested_cli_get'] = tool('orbit.task.artifact.get', named, cwd=nested)
    report['nested_mcp_get'] = mcp([('orbit_task_artifact_get', named)], cwd=nested)[0]
    report['nested_cli_put'] = tool('orbit.task.artifact.put', dict(put, workspace=selector),
                                    cwd=nested)
    report['cli_put'] = tool('orbit.task.artifact.put', put)
    report['cli_put_replay'] = tool('orbit.task.artifact.put', put)
    open(lose, 'w').close()
    report['lost_put'] = tool('orbit.task.artifact.put', put)
    report['retry_put'] = tool('orbit.task.artifact.put', put)
    report['wrong_path'] = tool('orbit.task.artifact.get', {'id': task, 'path': REPORT})
    report['cross_task'] = tool('orbit.task.artifact.get', {'id': 'TSO-999', 'path': MANIFEST})
    for name in ('stale', 'link', 'oversize'):
        key = {'stale': 'stale_report', 'link': 'symlink', 'oversize': 'oversize'}[name]
        report[key] = tool('orbit.task.artifact.put', dict(put, source_path=os.path.join(scratch, name + '.json')))
    report['unrelated'] = tool('orbit.task.show', {'id': task})
    content = __import__('base64').b64encode(open(os.path.join(scratch, 'report.json'), 'rb').read()).decode()
    report['forged'] = {
        'claim_override': raw('orbit.task.artifact.put', {'id': task, 'path': REPORT,
            'content_base64': content, 'claim_id': 'forged-claim'}),
        'workspace_override': raw('orbit.task.artifact.get', {'id': task, 'path': MANIFEST,
            'workspace': 'another-owner/ws_other'}),
        'certificate_path': raw('orbit.task.artifact.put', {'id': task,
            'path': 'review-certificate.json', 'content_base64': content}),
        'other_task': raw('orbit.task.artifact.put', {'id': 'TSO-999', 'path': REPORT,
            'content_base64': content}),
        'unrelated_tool': raw('orbit.task.update', {'id': task, 'status': 'done'}),
        'source_path': raw('orbit.task.artifact.put', {'id': task, 'path': REPORT,
            'source_path': '/etc/passwd'}),
    }
elif mode == 'calls':
    scratch = sys.argv[4]
    get = {'id': task, 'path': MANIFEST}
    put = {'id': task, 'path': REPORT, 'source_path': os.path.join(scratch, 'report.json')}
    report['cli_get'] = tool('orbit.task.artifact.get', get)
    report['cli_put'] = tool('orbit.task.artifact.put', put)
    report['mcp_get'], report['mcp_put'] = mcp([('orbit_task_artifact_get', get),
                                                ('orbit_task_artifact_put', put)])
else:
    get = {'id': task, 'path': MANIFEST}
    report['finished'] = tool('orbit.task.artifact.get', get)
    report['other_activity'] = tool('orbit.task.artifact.get', get,
        with_broker(os.environ['ORBIT_TEST_IMPLEMENTER_BROKER']))
    report['broker_gone'] = tool('orbit.task.artifact.get', get,
        with_broker(os.environ['ORBIT_TEST_DEAD_BROKER']))
    report['broker_gone_mcp'] = mcp([('orbit_task_artifact_get', get)],
        env=with_broker(os.environ['ORBIT_TEST_DEAD_BROKER']))[0]
print(json.dumps(report))
"#;
