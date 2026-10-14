//! Real process coverage through installed binaries and persisted generation state.
use super::*;
use orbit_common::fs::generation::executable_generation;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use orbit_common::fs::generation::{
    Access, GenerationGuard, Participant, ParticipantRole, QUIESCE_TIMEOUT_ENV,
};

/// Spawn an `orbit` binary that this fixture just copied into place.
///
/// On Linux this absorbs the parallel-fork `ETXTBSY` race described in
/// [`orbit_common::test_process`]. Any other spawn error returns on the first
/// attempt. Other platforms spawn once.
fn spawn_copied_orbit(command: &mut Command) -> std::io::Result<ChildGuard> {
    #[cfg(target_os = "linux")]
    {
        orbit_common::test_process::retry_executable_busy(|| command.spawn()).map(ChildGuard::new)
    }
    #[cfg(not(target_os = "linux"))]
    {
        command.spawn().map(ChildGuard::new)
    }
}

/// [`spawn_copied_orbit`] for a command run to completion.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn output_copied_orbit(command: &mut Command) -> std::io::Result<std::process::Output> {
    #[cfg(target_os = "linux")]
    {
        orbit_common::test_process::retry_executable_busy(|| command.output())
    }
    #[cfg(not(target_os = "linux"))]
    {
        command.output()
    }
}

fn preflight(workspace: &McpWorkspace) -> std::process::Output {
    McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["update", "--preflight", "--json"])
        .output()
        .expect("preflight")
}

fn candidate_preflight(workspace: &McpWorkspace, candidate: &Path) -> std::process::Output {
    McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["update", "--preflight", "--json", "--candidate"])
        .arg(candidate)
        .output()
        .expect("candidate preflight")
}

fn assert_refused(output: &std::process::Output) {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("upgrade admission refused"),
        "{output:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn distinct_candidate(workspace: &McpWorkspace) -> PathBuf {
    let candidate = workspace.home.join("candidate-orbit");
    crate::generation_fixture::distinct_copy(Path::new(env!("CARGO_BIN_EXE_orbit")), &candidate);
    candidate
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn authority_root(workspace: &McpWorkspace) -> PathBuf {
    workspace.home.join(".orbit")
}

fn store_bytes(workspace: &McpWorkspace) -> Vec<(PathBuf, Vec<u8>)> {
    let mut snapshots = Vec::new();
    for root in [workspace.home.join(".orbit"), workspace.work.join(".orbit")] {
        for name in [
            "orbit.db",
            "orbit.db-wal",
            "tasks/index.sqlite",
            "tasks/index.sqlite-wal",
            "state/semantic.db",
            "state/semantic.db-wal",
            "state/layout.version",
            "state/layout.compat",
        ] {
            let path = root.join(name);
            if path.is_file() {
                snapshots.push((path.clone(), std::fs::read(path).expect("snapshot")));
            }
        }
    }
    snapshots
}

fn audit_count(workspace: &McpWorkspace, tool: &str) -> i64 {
    let connection = Connection::open(workspace.home.join(".orbit/orbit.db")).expect("audit");
    connection
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE tool_name = ?1 AND status = 'success'",
            [tool],
            |r| r.get(0),
        )
        .expect("audit count")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn new_review_record_contract_refuses_a_live_old_client_before_mutation() {
    let workspace = McpWorkspace::init();
    let initialized = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args(["task", "list"]),
    );
    assert!(initialized.status.success());
    let latest = orbit_core::composition::compiled_compatibility();
    assert_eq!(latest.features.get("review"), Some(&4));
    let stored_version = Connection::open(workspace.home.join(".orbit/orbit.db"))
        .expect("review feature schema")
        .query_row(
            "SELECT MAX(version) FROM feature_schema_meta WHERE feature = 'review'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .expect("review feature migration applied");
    assert_eq!(stored_version, 4);
    let mut old_identity = latest.clone();
    old_identity.features.insert("review".into(), 3);

    // Hold the authority as a process compiled with the pre-change review
    // reader. The candidate CLI must not enter its store bootstrap while that
    // participant is live, because its reconciliation JSON is unreadable to
    // this client.
    let digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit")))
        .expect("fixture executable digest");
    let participant = Participant {
        digest: &digest,
        identity: &old_identity,
        role: ParticipantRole::McpServe,
        access: Access::Write,
        handover: None,
        in_activity: false,
    };
    let _old_client = GenerationGuard::join(
        &authority_root(&workspace),
        &participant,
        std::time::Duration::ZERO,
        || Ok(0),
    )
    .expect("pre-change client joins its own compatibility generation");

    let before = store_bytes(&workspace);
    let output = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .env(QUIESCE_TIMEOUT_ENV, "0")
        .args([
            "task",
            "add",
            "--title",
            "Must not be written during a review schema switch",
            "--complexity",
            "low",
            "--json",
        ])
        .output()
        .expect("candidate writer");
    assert_refused(&output);
    assert_eq!(
        store_bytes(&workspace),
        before,
        "incompatible admission must stop before store or workspace writes"
    );

    drop(_old_client);
    let added = orbit_ok(
        McpWorkspace::orbit_command(&workspace.work, &workspace.home).args([
            "task",
            "add",
            "--title",
            "Written after the old client quiesced",
            "--complexity",
            "low",
            "--json",
        ]),
    );
    let added: Value = serde_json::from_slice(&added.stdout).expect("task JSON");
    assert_eq!(added["title"], "Written after the old client quiesced");
}

/// An idle stdio MCP server refuses a plain preflight, which renames nothing,
/// but not an update that installs a candidate it can hand over to: `orbit
/// update --local-candidate` admits it, names it as handing over, renames the
/// candidate over the executable, and the server re-execs into it and keeps
/// serving the same session.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn persistent_client_hands_over_to_a_local_candidate_update() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let old = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &old).expect("copy installed executable");
    let mut command = McpWorkspace::orbit_program_command(&old, &workspace.work, &workspace.home);
    command
        .args([
            "mcp",
            "serve",
            "--operator",
            "--workspace",
            "ws_mcp-roundtrip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = spawn_copied_orbit(&mut command).expect("old server");
    let pid = child.id();
    let mut client = McpClient::new(child);
    workspace.initialize(&mut client);
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let schema = Connection::open(workspace.home.join(".orbit/orbit.db"))
        .expect("schema")
        .query_row(
            "SELECT COUNT(*) FROM schema_meta WHERE key = 'migration.v0021'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .expect("v21 ledger");
    assert_eq!(schema, 1);
    let contract = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["update", "--contract", "--json"])
        .output()
        .expect("contract probe");
    assert!(contract.status.success(), "{contract:?}");
    let report: Value = serde_json::from_slice(&contract.stdout).expect("contract JSON");
    assert_eq!(report["contract"], "executable-generation-v1");
    assert_eq!(report["admission_contract"], "compatibility-generation-v2");
    // A preflight without a candidate renames nothing, so nothing would hand
    // the session over; its remedy names the updates that would.
    let plain = preflight(&workspace);
    assert_refused(&plain);
    let refusal = String::from_utf8_lossy(&plain.stderr);
    assert!(
        refusal.contains(&format!("pid {pid} (mcp serve")),
        "{refusal}"
    );
    assert!(
        refusal.contains("`orbit update --local-candidate`"),
        "the remedy must name an update that admits the handover: {refusal}"
    );
    let candidate = distinct_candidate(&workspace);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    let admitted = candidate_preflight(&workspace, &candidate);
    assert!(admitted.status.success(), "{admitted:?}");
    let report: Value = serde_json::from_slice(&admitted.stdout).expect("preflight JSON");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["reservation"], false);
    let handover = report["handover"].as_array().expect("handover list");
    assert_eq!(handover.len(), 1, "{report}");
    assert_eq!(handover[0]["pid"], pid, "{report}");

    let task = client.call_tool_ok("orbit_task_add", json!({"title":"Before the update", "description":"Same live authority", "complexity":"low", "model":"codex"}));
    assert_eq!(task["title"], "Before the update");
    assert_eq!(audit_count(&workspace, "orbit.task.add"), 1);

    // The real update installs the candidate beside the idle session.
    let manifest = write_candidate_manifest(&workspace, &candidate);
    let output = local_candidate_update(&workspace, &candidate, &manifest, &old);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("update report");
    assert_eq!(report["outcome"], "updated", "{report}");
    let handover = report["handover"].as_array().expect("handover list");
    assert_eq!(handover.len(), 1, "{report}");
    assert_eq!(handover[0]["pid"], pid, "{report}");
    assert_eq!(handover[0]["role"], "mcp_serve", "{report}");
    assert_eq!(handover[0]["resume"], "mcp-stdio-v1", "{report}");
    assert_eq!(
        executable_generation(&old).expect("installed digest"),
        new_digest
    );
    // The update pins only once the session has handed over; the resumed
    // image then joins the generation it pinned.
    test_env::wait_until(
        "the handed-over MCP server to join as the candidate",
        || running_digest(&workspace, pid).as_deref() == Some(new_digest.as_str()),
    );
    assert_eq!(client.child.id(), pid);
    let after = client.call_tool_ok(
        "orbit_task_add",
        json!({"title":"After the update", "description":"Same session, new image", "complexity":"low", "model":"codex"}),
    );
    assert_eq!(after["title"], "After the update");
    assert_eq!(audit_count(&workspace, "orbit.task.add"), 2);
    drop(client);
    let ready = preflight(&workspace);
    assert!(ready.status.success(), "{ready:?}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn candidate_ok(workspace: &McpWorkspace, program: &Path, args: &[&str]) -> std::process::Output {
    let mut command =
        McpWorkspace::orbit_program_command(program, &workspace.work, &workspace.home);
    command.args(args).stdin(Stdio::null());
    let output = output_copied_orbit(&mut command).expect("orbit launch");
    assert!(
        output.status.success(),
        "{args:?} must be admitted beside the live processes: {output:?}"
    );
    output
}

#[test]
fn refusal_during_concurrent_calls_and_after_discarded_mutation_reply_never_replays() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::upgrade::refusal_during_concurrent_calls_and_after_discarded_mutation_reply_never_replays",
    ) {
        return;
    }
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let connection = Connection::open(workspace.home.join(".orbit/orbit.db")).expect("audit lock");
    connection
        .execute_batch("BEGIN IMMEDIATE")
        .expect("hold audit writes");
    // Requests are outstanding at the client while the audit writer is held.
    for id in [1001, 1002] {
        client.send(&json!({"jsonrpc":"2.0", "id":id, "method":"tools/call", "params":{"name":"orbit_task_add", "arguments":{"title":format!("Outstanding {id}"), "description":"No replay", "complexity":"low", "model":"codex"}}}));
    }
    assert_refused(&preflight(&workspace));
    connection
        .execute_batch("COMMIT")
        .expect("release audit writes");
    // Consume and deliberately discard both replies, modelling a caller that
    // has no result to retry safely. Upgrade must never resubmit either call.
    let mut received = BTreeSet::new();
    while received.len() < 2 {
        let reply: Value = serde_json::from_str(
            &client
                .lines
                .recv_timeout(RESPONSE_TIMEOUT)
                .expect("mutation reply"),
        )
        .expect("JSON reply");
        if let Some(id) = reply["id"].as_i64() {
            assert_eq!(reply["result"]["isError"], false, "{reply}");
            received.insert(id);
        }
    }
    assert_refused(&preflight(&workspace));
    client.call_tool_ok("orbit_workspace_list", json!({}));
    assert_eq!(audit_count(&workspace, "orbit.task.add"), 2);
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(tasks["total"], 2);
}

#[test]
fn listener_retains_admission_until_process_exit() {
    let workspace = McpWorkspace::init();
    let (mut client, _) = workspace.listen();
    assert_refused(&preflight(&workspace));
    // The listener is never handed over, so even a candidate cannot admit it.
    let refused = candidate_preflight(&workspace, Path::new(env!("CARGO_BIN_EXE_orbit")));
    assert_refused(&refused);
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(refusal.contains("(mcp listen, started"), "{refusal}");
    assert!(
        refusal.contains("TCP listener is never handed over"),
        "{refusal}"
    );
    client.call_tool_ok("orbit_workspace_list", json!({}));
    drop(client);
    assert!(preflight(&workspace).status.success());
}

/// The source commit every local candidate here is attested to.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const CANDIDATE_COMMIT: &str = "0d0e0a0d0b0e0e0f0d0e0a0d0b0e0e0f0d0e0a0d";

/// Describe `candidate` in a new manifest beside it, through the candidate.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_candidate_manifest(workspace: &McpWorkspace, candidate: &Path) -> PathBuf {
    let manifest = candidate.with_extension("json");
    let (candidate_arg, manifest_arg) = (
        candidate.to_string_lossy().into_owned(),
        manifest.to_string_lossy().into_owned(),
    );
    candidate_ok(
        workspace,
        candidate,
        &[
            "update",
            "--local-candidate",
            &candidate_arg,
            "--source-commit",
            CANDIDATE_COMMIT,
            "--write-candidate-manifest",
            &manifest_arg,
        ],
    );
    manifest
}

/// Install `candidate` over `installed` with `orbit update --local-candidate`,
/// run through the candidate itself as the runbook does. The update probes the
/// copy it stages, which macOS first assesses as a new executable while the
/// probe's clock runs; on a loaded host that outlasted the 30 s default, so the
/// probe bound here is only a hang guard.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn local_candidate_update(
    workspace: &McpWorkspace,
    candidate: &Path,
    manifest: &Path,
    installed: &Path,
) -> std::process::Output {
    let mut command =
        McpWorkspace::orbit_program_command(candidate, &workspace.work, &workspace.home);
    command
        .env(
            "ORBIT_INSTALL_DIR",
            installed.parent().expect("install dir"),
        )
        .env(
            orbit_cmd::update::converge::PROBE_TIMEOUT_ENV,
            test_env::FIXTURE_STEP_DEADLINE.as_secs().to_string(),
        )
        .args(["update", "--local-candidate"])
        .arg(candidate)
        .arg("--candidate-manifest")
        .arg(manifest)
        .args(["--source-commit", CANDIDATE_COMMIT, "--install-target"])
        .arg(installed)
        .arg("--json")
        .stdin(Stdio::null());
    output_copied_orbit(&mut command).expect("local candidate update")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn running_digest(workspace: &McpWorkspace, pid: u32) -> Option<String> {
    crate::generation_fixture::running_digest(&authority_root(workspace), pid)
}

/// Settle the detached fixture worker even when a handover assertion unwinds.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct DrainCleanup<'a> {
    workspace: &'a McpWorkspace,
    run_id: String,
    armed: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for DrainCleanup<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut command = McpWorkspace::orbit_command(&self.workspace.work, &self.workspace.home);
        command.args(["run", "cancel", &self.run_id, "--confirm"]);
        // Production cancellation waits for the verified owner to exit. Keep
        // cleanup bounded without a second panic during assertion unwinding.
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            test_env::FIXTURE_STEP_DEADLINE,
            64 * 1024,
        );
        if !matches!(&output, Ok(output) if output.status.success()) {
            tracing::warn!(?output, "drain fixture cleanup failed");
        }
    }
}

/// Candidate admission and MCP session continuity do not exercise a drain's
/// run adoption. Keep that distinct process contract as one finite fixture.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_replaced_drain_hands_its_run_to_the_installed_executable() {
    if crate::isolated_test::run_in_child(
        "mcp_roundtrip::upgrade::a_replaced_drain_hands_its_run_to_the_installed_executable",
    ) {
        return;
    }
    let workspace = McpWorkspace::init();
    let installed = workspace.home.join("installation/orbit");
    std::fs::create_dir_all(installed.parent().expect("install directory")).expect("installation");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("installed executable");
    let old_digest = executable_generation(&installed).expect("old digest");
    let submitted = candidate_ok(
        &workspace,
        &installed,
        &[
            "job",
            "run",
            "workspace_auto_pipeline",
            "--input",
            "for_seconds=300",
            "--input",
            "idle_sleep_seconds=1",
            "--input",
            "poll_sleep_seconds=1",
            "--json",
        ],
    );
    let submitted: Value = serde_json::from_slice(&submitted.stdout).expect("submission");
    let mut drain = DrainCleanup {
        workspace: &workspace,
        run_id: submitted["run_id"].as_str().expect("run ID").to_owned(),
        armed: true,
    };
    let show = || {
        let output = candidate_ok(
            &workspace,
            Path::new(env!("CARGO_BIN_EXE_orbit")),
            &["run", "show", &drain.run_id, "--json"],
        );
        serde_json::from_slice::<Value>(&output.stdout).expect("run JSON")
    };
    let mut before = Value::Null;
    test_env::wait_until("the drain's first admission pass", || {
        before = show();
        before["run"]["state"] == "running"
            && before["pipeline_state"]["drain_last_pass"]["recorded_at"].is_string()
    });
    let pid =
        u32::try_from(before["run"]["pid"].as_u64().expect("worker PID")).expect("PID fits u32");
    assert_eq!(running_digest(&workspace, pid), Some(old_digest.clone()));
    let candidate = distinct_candidate(&workspace);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    assert_ne!(new_digest, old_digest, "the fixture must replace the image");
    install_prepared_candidate(&candidate, &installed);
    test_env::wait_until("the drain to re-exec the changed installation", || {
        assert!(
            orbit_common::process::identity::process_is_alive(pid),
            "drain exited before adoption"
        );
        running_digest(&workspace, pid).as_deref() == Some(new_digest.as_str())
    });

    // A registration alone can precede adoption. Require a *later* admission
    // pass from the new image, so dropping ADOPT_RUN_ENV cannot race a cancel
    // and masquerade as a successful handover.
    let registered = show();
    let mut adopted = Value::Null;
    test_env::wait_until("the adopted image to continue admission", || {
        adopted = show();
        assert_eq!(adopted["run"]["state"], "running", "{adopted}");
        adopted["pipeline_state"]["drain_last_pass"]["recorded_at"]
            != registered["pipeline_state"]["drain_last_pass"]["recorded_at"]
    });
    assert_eq!(adopted["run"]["run_id"], drain.run_id);
    assert_eq!(adopted["run"]["pid"], u64::from(pid));
    assert_eq!(adopted["run"]["started_at"], before["run"]["started_at"]);
    assert_eq!(running_digest(&workspace, pid), Some(new_digest));
    candidate_ok(
        &workspace,
        &installed,
        &["run", "cancel", &drain.run_id, "--confirm"],
    );
    test_env::wait_until("the cancelled adopted owner to exit", || {
        !orbit_common::process::identity::process_is_alive(pid)
    });
    assert_eq!(show()["run"]["state"], "cancelled");
    // Keep unwind cleanup until both cancellation and owner exit are proven,
    // then avoid launching a second cancellation for the already-finished run.
    drain.armed = false;
}

/// `distinct_candidate` already staged and assessed this disposable image.
/// Rename that same inode over the installation, leaving the running old inode
/// intact. Copying and assessing it a second time adds no handover coverage.
/// The local-candidate update row still exercises the production installer.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn install_prepared_candidate(candidate: &Path, installed: &Path) {
    std::fs::rename(candidate, installed).expect("install prepared candidate");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_replaced_mcp_server_defers_handover_until_a_large_partial_request_completes() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");
    let old_digest = executable_generation(&installed).expect("old digest");
    let mut command =
        McpWorkspace::orbit_program_command(&installed, &workspace.work, &workspace.home);
    command
        .args([
            "mcp",
            "serve",
            "--operator",
            "--workspace",
            "ws_mcp-roundtrip",
        ])
        .env("RUST_LOG", "orbit.mcp.handover=debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let child = spawn_copied_orbit(&mut command).expect("old server");
    let pid = child.id();
    let mut client = McpClient::new(child);
    workspace.initialize(&mut client);

    // Leave a valid tools/call incomplete. This prefix exceeds both the
    // 32 KiB handover limit and the pipe capacity, so writing it forces the
    // reader to buffer an oversized partial line before installation changes.
    let description = "x".repeat(128 * 1024);
    client.next_id += 1;
    let id = client.next_id;
    let mut line = serde_json::to_vec(&json!({
        "jsonrpc":"2.0", "id":id, "method":"tools/call",
        "params":{"name":"orbit_task_add", "arguments":{
            "title":"Completed before handover", "description":description,
            "complexity":"low", "model":"codex"
        }}
    }))
    .expect("request JSON");
    line.push(b'\n');
    let split = 112 * 1024;
    client
        .writer
        .write_all(&line[..split])
        .expect("partial request");
    client.writer.flush().expect("flush partial request");

    let candidate = distinct_candidate(&workspace);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    install_prepared_candidate(&candidate, &installed);

    // Observe actual deferred lifecycle checks rather than assuming five
    // seconds of wall time gave a busy host multiple chances to hand over.
    let log = workspace.home.join(".orbit/state/logs/orbit.jsonl");
    test_env::wait_until(
        "two handover checks to defer the oversized partial request",
        || {
            assert!(
                matches!(client.child.try_wait(), Ok(None)),
                "an oversized partial request must not yield the MCP session"
            );
            assert_eq!(
                running_digest(&workspace, pid).as_deref(),
                Some(old_digest.as_str()),
                "handover must wait for the oversized partial line to complete"
            );
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter(|event| {
                    event["target"] == "orbit.mcp.handover"
                        && event["fields"]["handover_deferred"] == true
                })
                .take(2)
                .count()
                == 2
        },
    );

    client
        .writer
        .write_all(&line[split..])
        .expect("finish request");
    client.writer.flush().expect("flush completed request");
    let reply: Value = serde_json::from_str(
        &client
            .lines
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("completed request reply"),
    )
    .expect("reply JSON");
    assert_eq!(reply["id"], id, "{reply}");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert_eq!(
        reply["result"]["structuredContent"]["description"],
        description
    );

    // Once the call is answered, a later idle check can resume on the new
    // image using the original process, pipes, and initialize parameters.
    test_env::wait_until(
        "the MCP session to hand over after completing the partial request",
        || running_digest(&workspace, pid).as_deref() == Some(new_digest.as_str()),
    );
    assert_eq!(client.child.id(), pid);
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(
        tasks["total"], 1,
        "the completed request must not be replayed"
    );
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_replaced_mcp_server_hands_its_session_over_after_invalid_requests() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");
    let old_digest = executable_generation(&installed).expect("old digest");
    let stderr_path = workspace.home.join("invalid-handover.stderr");
    let stderr = std::fs::File::create(&stderr_path).expect("server stderr");
    let mut command =
        McpWorkspace::orbit_program_command(&installed, &workspace.work, &workspace.home);
    command
        .args([
            "mcp",
            "serve",
            "--operator",
            "--workspace",
            "ws_mcp-roundtrip",
        ])
        .env("RUST_LOG", "orbit.generation=debug,orbit.mcp=debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    let child = spawn_copied_orbit(&mut command).expect("old server");
    let pid = child.id();
    let mut client = McpClient::new(child);
    workspace.initialize(&mut client);
    client.call_tool_ok(
        "orbit_task_add",
        json!({"title":"Before the handover", "description":"Old image", "complexity":"low", "model":"codex"}),
    );
    assert_eq!(
        running_digest(&workspace, pid).as_deref(),
        Some(old_digest.as_str())
    );

    // Well-formed JSON that rmcp cannot decode receives an id-less error.
    // None of these ids may pin the session forever, including batch input
    // (the stdio transport accepts individual messages only).
    for invalid in [
        json!({"jsonrpc":"2.0", "id":1001, "method":"tools/call", "params":"x"}),
        json!({"jsonrpc":"1.0", "id":1002, "method":"ping"}),
        json!([{"jsonrpc":"2.0", "id":1003, "method":"ping"}]),
    ] {
        client.send(&invalid);
        let reply: Value = serde_json::from_str(
            &client
                .lines
                .recv_timeout(RESPONSE_TIMEOUT)
                .expect("rejection"),
        )
        .expect("JSON rejection");
        assert!(reply.get("id").is_none(), "{reply}");
        assert_eq!(reply["error"]["code"], -32600, "{reply}");
    }
    // The compatibility path silently drops an undecodable non-standard
    // notification even when the sender included an id. A subsequent ping
    // proves the transport consumed it before the replacement is installed.
    client
        .send(&json!({"jsonrpc":"2.0", "id":1004, "method":"notifications/custom", "params":"x"}));
    let ping = client.request("ping", Value::Null);
    assert_eq!(ping["result"], json!({}), "{ping}");

    let candidate = distinct_candidate(&workspace);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    install_prepared_candidate(&candidate, &installed);

    // The idle server notices within a lifecycle interval and execs itself.
    let started = Instant::now();
    test_env::wait_until(
        "the idle server to hand over to the installed executable",
        || {
            assert!(
                started.elapsed() < test_env::FIXTURE_STEP_DEADLINE,
                "idle handover did not finish: {}",
                std::fs::read_to_string(&stderr_path).unwrap_or_default()
            );
            assert!(
                client.child.try_wait().expect("poll server").is_none(),
                "the server exited instead of handing over: {}",
                std::fs::read_to_string(&stderr_path).unwrap_or_default()
            );
            running_digest(&workspace, pid).as_deref() == Some(new_digest.as_str())
        },
    );

    // Same process, same pipes, no second `initialize`: the session goes on.
    assert_eq!(client.child.id(), pid);
    let task = client.call_tool_ok(
        "orbit_task_add",
        json!({"title":"After the handover", "description":"New image", "complexity":"low", "model":"codex"}),
    );
    assert_eq!(task["title"], "After the handover");
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(tasks["total"], 2, "{tasks}");
    assert_eq!(
        running_digest(&workspace, pid).as_deref(),
        Some(new_digest.as_str())
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn upgrade_admission_and_session_handover() {
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .finish();
    let _trace = tracing::subscriber::set_default(subscriber);
    for (name, check) in [
        (
            "new_review_record_contract_refuses_a_live_old_client_before_mutation",
            new_review_record_contract_refuses_a_live_old_client_before_mutation as fn(),
        ),
        (
            "persistent_client_hands_over_to_a_local_candidate_update",
            persistent_client_hands_over_to_a_local_candidate_update as fn(),
        ),
        (
            "a_replaced_mcp_server_defers_handover_until_a_large_partial_request_completes",
            a_replaced_mcp_server_defers_handover_until_a_large_partial_request_completes as fn(),
        ),
    ] {
        tracing::info!(case = name, "MCP contract row");
        let started = Instant::now();
        check();
        tracing::info!(case = name, elapsed = ?started.elapsed(), "MCP contract row completed");
    }
}
