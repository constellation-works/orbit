//! Real process coverage through installed binaries and persisted generation state.
use super::*;
use orbit_common::fs::generation::executable_generation;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use orbit_common::fs::generation::{
    Access, GenerationGuard, Participant, ParticipantRole, QUIESCE_TIMEOUT_ENV,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::BTreeMap;

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn generation_record(workspace: &McpWorkspace) -> String {
    std::fs::read_to_string(authority_root(workspace).join(".generation.lock")).expect("record")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries {
            let entry = entry.expect("entry");
            let path = entry.path();
            let file_type = entry.file_type().expect("file type");
            if file_type.is_dir() {
                walk(&path, files);
            } else if file_type.is_file() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                // SQLite may materialize WAL/SHM for a read-only open. Flock
                // state (and the empty lock files flock needs) is excluded.
                // Participant registrations are admission state too.
                if path
                    .components()
                    .any(|component| component.as_os_str() == ".generation-participants")
                    || name.ends_with("-wal")
                    || name.ends_with("-shm")
                    || name.ends_with(".lock")
                    || name == "orbit.jsonl"
                    || name.starts_with("orbit.jsonl.")
                {
                    continue;
                }
                files.insert(path.clone(), std::fs::read(&path).expect("read"));
            }
        }
    }
    walk(root, &mut files);
    files
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

#[test]
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
#[test]
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

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_newer_build_writes_while_older_mcp_dashboard_and_drain_processes_stay_live() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let old = Path::new(env!("CARGO_BIN_EXE_orbit"));
    let (mut dashboard, port) = spawn_dashboard(&workspace, old);
    let drain = start_drain(&workspace, old);
    // Distinct bytes, same store schema, layout and feature schemas: admission
    // keys on compatibility, so the candidate joins the live generation as a
    // writer instead of being refused for its digest.
    let candidate = distinct_candidate(&workspace);
    let old_digest = executable_generation(old).expect("old");
    assert_ne!(
        executable_generation(&candidate).expect("candidate"),
        old_digest
    );
    assert_eq!(
        running_digest(&workspace, drain.pid).as_deref(),
        Some(old_digest.as_str())
    );
    let recorded = generation_record(&workspace);
    let added = candidate_ok(
        &workspace,
        &candidate,
        &[
            "task",
            "add",
            "--title",
            "Written by the candidate",
            "--complexity",
            "low",
            "--json",
        ],
    );
    let added: Value = serde_json::from_slice(&added.stdout).expect("task add JSON");
    let task_id = added["id"].as_str().expect("task id").to_string();
    for args in [
        vec!["migrate", "--confirm"],
        vec!["workspace", "sync"],
        vec![
            "task",
            "update",
            &task_id,
            "--title",
            "Updated by the candidate",
        ],
        vec!["clock", "tick"],
    ] {
        candidate_ok(&workspace, &candidate, &args);
    }
    assert_eq!(
        generation_record(&workspace),
        recorded,
        "a compatible writer joins without taking the generation over"
    );

    // Every older long-lived process is still up, on its own image, serving.
    assert!(
        matches!(dashboard.try_wait(), Ok(None)),
        "the dashboard must stay live"
    );
    assert!(http_get(port, "/healthz").contains("ok"));
    let run = run_show(&workspace, &drain.run_id);
    assert_eq!(run["run"]["state"], "running", "{run}");
    assert_eq!(run["run"]["pid"].as_u64(), Some(u64::from(drain.pid)));
    assert_eq!(
        running_digest(&workspace, drain.pid).as_deref(),
        Some(old_digest.as_str())
    );
    let task = client.call_tool_ok(
        "orbit_task_add",
        json!({"title":"Written by the old client", "description":"After the candidate wrote", "complexity":"low", "model":"codex"}),
    );
    assert_eq!(task["title"], "Written by the old client");
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(tasks["total"], 2, "{tasks}");
    // `orbit update` still needs the whole authority to itself.
    assert_refused(&preflight(&workspace));

    cancel_drain(&workspace, &drain);
    stop(&mut dashboard);
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_replaced_drain_hands_its_run_to_the_installed_executable() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");
    let drain = start_drain(&workspace, &installed);
    assert_eq!(
        running_digest(&workspace, drain.pid),
        Some(executable_generation(&installed).expect("old digest"))
    );

    let candidate = distinct_candidate(&workspace);
    let new_digest = executable_generation(&candidate).expect("candidate digest");
    crate::generation_fixture::install_over(&candidate, &installed);

    // The coordinator notices at its next admission pass and execs in place.
    test_env::wait_until("the drain to hand over to the installed executable", || {
        running_digest(&workspace, drain.pid).as_deref() == Some(new_digest.as_str())
    });
    // Same run, same owner: the new image adopted it rather than claiming it,
    // and it is still running a few admission passes later.
    std::thread::sleep(Duration::from_secs(3));
    let adopted = run_show(&workspace, &drain.run_id);
    assert_eq!(adopted["run"]["state"], "running", "{adopted}");
    assert_eq!(adopted["run"]["pid"].as_u64(), Some(u64::from(drain.pid)));
    cancel_drain(&workspace, &drain);
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_pending_breaking_switch_waits_for_live_processes_to_yield_at_safe_points() {
    use orbit_common::fs::generation::{Access, GenerationUpdate, Participant, ParticipantRole};

    let workspace = McpWorkspace::init();
    let jobs = workspace.home.join(".orbit/resources/jobs");
    std::fs::create_dir_all(&jobs).expect("job catalog");
    std::fs::write(
        jobs.join("quiesce_fixture.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: quiesce_fixture\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: first\n      default_input:\n        seconds: 8\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n    - id: second\n      default_input:\n        seconds: 30\n      spec:\n        type: deterministic\n        action: sleep\n        config: {}\n",
    )
    .expect("fixture job");
    let submitted = candidate_ok(
        &workspace,
        Path::new(env!("CARGO_BIN_EXE_orbit")),
        &["job", "run", "quiesce_fixture", "--json"],
    );
    let run_id = serde_json::from_slice::<Value>(&submitted.stdout).expect("submission")["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let worker = wait_for_owner(&workspace, &run_id);
    let mut client = workspace.serve();
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let (mut dashboard, _) = spawn_dashboard(&workspace, Path::new(env!("CARGO_BIN_EXE_orbit")));

    // A build whose store migration breaks older writers.
    let current = orbit_core::composition::compiled_compatibility();
    let mut breaking = current.clone();
    breaking.store_schema.version += 1;
    breaking.store_schema.writer_floor = breaking.store_schema.version;
    breaking.store_schema.reader_floor = breaking.store_schema.version;
    let breaking_digest = "b".repeat(64);
    let participant = Participant {
        digest: &breaking_digest,
        identity: &breaking,
        role: ParticipantRole::Command,
        access: Access::Write,
        handover: None,
        in_activity: false,
    };
    let root = authority_root(&workspace);

    // Bound expires mid-step: the refusal names the worker that blocks it.
    let refused = GenerationGuard::join(&root, &participant, Duration::from_millis(500), || Ok(0))
        .err()
        .expect("the switch cannot complete while the worker is mid-step")
        .to_string();
    assert!(
        refused.contains(&format!("pid {} (drain, started ", worker)),
        "{refused}"
    );

    // While the switch is pending, no new old-generation participant joins.
    let late_joiner = {
        let workspace_home = workspace.home.clone();
        let work = workspace.work.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            McpWorkspace::orbit_program_command(
                Path::new(env!("CARGO_BIN_EXE_orbit")),
                &work,
                &workspace_home,
            )
            .args(["task", "list"])
            .stdin(Stdio::null())
            .output()
            .expect("late joiner")
        })
    };
    let admitted = GenerationGuard::join(&root, &participant, Duration::from_secs(60), || Ok(0))
        .expect("the worker yields at its step boundary and the switch proceeds");
    let late = late_joiner.join().expect("late joiner thread");
    assert!(!late.status.success(), "{late:?}");
    assert!(
        String::from_utf8_lossy(&late.stderr).contains("generation switch is pending"),
        "{late:?}"
    );
    // The idle MCP server and dashboard yielded too: the switch could not
    // have been admitted while either still held its share.
    assert!(
        matches!(client.child.try_wait(), Ok(Some(_))),
        "mcp serve yields"
    );
    assert!(
        matches!(dashboard.try_wait(), Ok(Some(_))),
        "the dashboard yields"
    );
    drop(admitted);
    // Roll the simulated candidate back so the fixture's own binary can read
    // what the worker recorded; the store itself never migrated.
    let old_digest = executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("old");
    drop(
        GenerationUpdate::acquire(&root)
            .expect("nothing is live")
            .pin(&old_digest, Some(&current))
            .expect("roll back"),
    );

    // The worker stopped after its first step and recorded why, as
    // interrupted rather than failed, so the run resumes from its checkpoint.
    let run = poll_run_state(&workspace, &run_id, "interrupted");
    let steps = run["run"]["steps"].as_array().expect("steps");
    assert!(
        steps
            .iter()
            .any(|step| step["error_code"] == "upgrade_quiesce"),
        "{run}"
    );
    assert!(!process_alive(worker), "the yielding worker exits");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Drain {
    run_id: String,
    pid: u32,
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_show(workspace: &McpWorkspace, run_id: &str) -> Value {
    let output = candidate_ok(
        workspace,
        Path::new(env!("CARGO_BIN_EXE_orbit")),
        &["run", "show", run_id, "--json"],
    );
    serde_json::from_slice(&output.stdout).expect("run show JSON")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn poll_run_state(workspace: &McpWorkspace, run_id: &str, state: &str) -> Value {
    let mut last = Value::Null;
    test_env::wait_until(&format!("run {run_id} to be {state}"), || {
        last = run_show(workspace, run_id);
        last["run"]["state"] == state
    });
    last
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_owner(workspace: &McpWorkspace, run_id: &str) -> u32 {
    let run = poll_run_state(workspace, run_id, "running");
    run["run"]["pid"].as_u64().expect("worker pid") as u32
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn start_drain(workspace: &McpWorkspace, program: &Path) -> Drain {
    let submitted = candidate_ok(
        workspace,
        program,
        &[
            "job",
            "run",
            "workspace_auto_pipeline",
            "--input",
            "for_seconds=600",
            "--input",
            "idle_sleep_seconds=1",
            "--input",
            "poll_sleep_seconds=1",
            "--json",
        ],
    );
    let run_id = serde_json::from_slice::<Value>(&submitted.stdout).expect("submission")["run_id"]
        .as_str()
        .expect("run id")
        .to_string();
    let pid = wait_for_owner(workspace, &run_id);
    Drain { run_id, pid }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cancel_drain(workspace: &McpWorkspace, drain: &Drain) {
    candidate_ok(
        workspace,
        Path::new(env!("CARGO_BIN_EXE_orbit")),
        &["run", "cancel", &drain.run_id, "--confirm"],
    );
    test_env::wait_until("the cancelled drain to exit", || !process_alive(drain.pid));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_alive(pid: u32) -> bool {
    orbit_common::process::identity::process_is_alive(pid)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_dashboard(workspace: &McpWorkspace, program: &Path) -> (ChildGuard, u16) {
    let port = TcpListener::bind(("127.0.0.1", 0))
        .expect("ephemeral port")
        .local_addr()
        .expect("local addr")
        .port();
    let child = McpWorkspace::orbit_program_command(program, &workspace.work, &workspace.home)
        .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(ChildGuard::new)
        .expect("spawn dashboard");
    test_env::wait_until("the dashboard to listen", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    (child, port)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn stop(child: &mut ChildGuard) {
    // Safety: SIGTERM to this test's own child, as a service manager would.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !matches!(child.try_wait(), Ok(Some(_))) {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn refusal_during_concurrent_calls_and_after_discarded_mutation_reply_never_replays() {
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
    let listener = TcpListener::bind("127.0.0.1:0").expect("free port");
    let addr = listener.local_addr().expect("address");
    drop(listener);
    let mut client = workspace.listen(addr);
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn audit_rows(workspace: &McpWorkspace) -> i64 {
    Connection::open_with_flags(
        workspace.home.join(".orbit/orbit.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("audit")
    .query_row("SELECT COUNT(*) FROM audit_events", [], |row| row.get(0))
    .expect("audit count")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_byte_identical_root(
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
) {
    let before_keys: BTreeSet<_> = before.keys().collect();
    let after_keys: BTreeSet<_> = after.keys().collect();
    let added: Vec<_> = after_keys.difference(&before_keys).collect();
    let removed: Vec<_> = before_keys.difference(&after_keys).collect();
    assert!(added.is_empty(), "read-only join created files: {added:?}");
    assert!(
        removed.is_empty(),
        "read-only join removed files: {removed:?}"
    );
    for (path, bytes) in before {
        assert_eq!(
            after.get(path).map(Vec::as_slice),
            Some(bytes.as_slice()),
            "read-only join changed {}",
            path.display()
        );
    }
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_only_candidate_joins_a_v1_generation_without_rewriting_the_record() {
    let workspace = McpWorkspace::init();
    {
        let mut client = workspace.serve();
        client.call_tool_ok("orbit_workspace_list", json!({}));
        drop(client);
    }
    // A live executable-generation-v1 process: it records only its digest and
    // never yields, so v2 builds fall back to v1 rules against it.
    const V1_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let _pin_a = GenerationGuard::acquire(&authority_root(&workspace), V1_DIGEST).expect("pin A");
    let recorded = generation_record(&workspace);
    let candidate = distinct_candidate(&workspace);
    assert_ne!(
        executable_generation(&candidate).expect("candidate"),
        executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("live")
    );
    let before_tree = snapshot_tree(&authority_root(&workspace));
    let before_audit = audit_rows(&workspace);

    for args in [
        vec!["task", "list", "--json"],
        vec!["task", "flow"],
        vec!["run", "history"],
        vec!["run", "show"],
        vec!["search", "registry"],
        vec!["workspace", "list"],
        vec!["workspace", "show"],
        vec!["tool", "list"],
    ] {
        let output =
            McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home)
                .args(&args)
                .output()
                .expect("read-only candidate");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("upgrade admission refused"),
            "read-only {:?} must be admitted under a live foreign pin\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            stderr
        );
        assert!(
            output.status.success()
                || stderr.contains("not found")
                || stderr.contains("job run not found"),
            "read-only {:?} failed after admission\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            stderr
        );
    }

    let show = McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home)
        .args(["task", "show", "TST-1"])
        .output()
        .expect("task show");
    assert!(
        show.status.success() || String::from_utf8_lossy(&show.stderr).contains("not found"),
        "task show must pass admission: {}",
        String::from_utf8_lossy(&show.stderr)
    );

    assert_eq!(generation_record(&workspace), recorded);
    assert_byte_identical_root(&before_tree, &snapshot_tree(&authority_root(&workspace)));
    assert_eq!(audit_rows(&workspace), before_audit);

    let writes = McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home)
        .args(["task", "add", "--title", "nope", "--complexity", "low"])
        .output()
        .expect("writing candidate");
    assert_refused(&writes);
    assert!(
        String::from_utf8_lossy(&writes.stderr).contains("this command writes"),
        "{}",
        String::from_utf8_lossy(&writes.stderr)
    );

    let _joiner = GenerationGuard::acquire(&authority_root(&workspace), V1_DIGEST)
        .expect("hold a second v1 shared pin");
    assert_refused(&preflight(&workspace));
    assert_eq!(generation_record(&workspace), recorded);
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_only_foreign_digest_refuses_when_store_schema_differs() {
    let workspace = McpWorkspace::init();
    {
        let mut client = workspace.serve();
        client.call_tool_ok("orbit_workspace_list", json!({}));
        drop(client);
    }
    let home_orbit = authority_root(&workspace);
    const FOREIGN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let _pin = GenerationGuard::acquire(&home_orbit, FOREIGN).expect("pin A");
    let db = home_orbit.join("orbit.db");
    assert!(
        db.is_file(),
        "expected {} after MCP bootstrap",
        db.display()
    );
    let connection = Connection::open(&db).expect("store");
    connection
        .execute(
            "INSERT INTO schema_meta(key, value, updated_at) VALUES ('migration.v9999', 'fake', 'now')",
            [],
        )
        .expect("bump schema");
    drop(connection);
    let candidate = distinct_candidate(&workspace);
    let mut command =
        McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home);
    command.args(["task", "list", "--json"]);
    // The candidate was just written, so a sibling test's fork can still hold
    // it open for writing; see `orbit_common::test_process`.
    let output =
        crate::generation_fixture::launch(|| command.output()).expect("schema-mismatch candidate");
    assert_refused(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("store schema 9999 differs from compiled schema"),
        "{stderr}"
    );
    assert_eq!(generation_record(&workspace), format!("1:{FOREIGN}\n"));
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

#[test]
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    crate::generation_fixture::install_over(&candidate, &installed);

    // Hold the partial line across multiple lifecycle checks. The old image
    // must keep serving; yielding or handing over now would lose this call.
    let hold_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < hold_until {
        assert!(
            matches!(client.child.try_wait(), Ok(None)),
            "an oversized partial request must not yield the MCP session"
        );
        assert_eq!(
            running_digest(&workspace, pid).as_deref(),
            Some(old_digest.as_str()),
            "handover must wait for the oversized partial line to complete"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
    crate::generation_fixture::install_over(&candidate, &installed);

    // The idle server notices within a lifecycle interval and execs itself.
    test_env::wait_until(
        "the idle server to hand over to the installed executable",
        || running_digest(&workspace, pid).as_deref() == Some(new_digest.as_str()),
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

/// `orbit update --local-candidate` is the same guarded replacement as a
/// release. While processes that cannot hand over run the installed build —
/// a dashboard, then the dashboard and a drain coordinator beside an
/// initialized stdio MCP session with a request whose reply is still unread —
/// it refuses before the executable, the generation record or any store
/// changes, and every client carries on: the drain keeps its run and owner
/// rather than being interrupted for resume. Once they exit it installs, and
/// each client kind starts again from the installed candidate.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn a_local_candidate_refuses_live_clients_untouched_and_serves_them_once_they_reconnect() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let installed = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &installed).expect("install old executable");
    let old_digest = executable_generation(&installed).expect("old digest");
    let candidate = distinct_candidate(&workspace);
    let manifest = write_candidate_manifest(&workspace, &candidate);
    let local_update = || local_candidate_update(&workspace, &candidate, &manifest, &installed);
    let serve = |program: &Path| {
        let mut command =
            McpWorkspace::orbit_program_command(program, &workspace.work, &workspace.home);
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
            .stderr(Stdio::null());
        let mut client = McpClient::new(spawn_copied_orbit(&mut command).expect("mcp serve"));
        workspace.initialize(&mut client);
        client
    };

    let mut client = serve(&installed);
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let stores = store_bytes(&workspace);
    let record = generation_record(&workspace);
    // The idle session alone would hand over to the candidate; a registered
    // dashboard never does, so it refuses the update with its own remedy.
    let identity = orbit_core::composition::compiled_compatibility();
    let dashboard = GenerationGuard::join(
        &authority_root(&workspace),
        &Participant {
            digest: &old_digest,
            identity: &identity,
            role: ParticipantRole::Dashboard,
            access: Access::Write,
            handover: None,
            in_activity: false,
        },
        Duration::ZERO,
        || Ok(0),
    )
    .expect("the dashboard joins the installed generation");
    let refused = local_update();
    assert_refused(&refused);
    let refusal = String::from_utf8_lossy(&refused.stderr);
    assert!(refusal.contains("stop the dashboard"), "{refusal}");
    assert!(
        refusal.contains("hands over to the candidate"),
        "the idle session is named as handing over, not as blocking: {refusal}"
    );
    drop(dashboard);
    assert_eq!(
        store_bytes(&workspace),
        stores,
        "refusal touched store/layout bytes"
    );
    // The session's own writes race a byte snapshot once a request is in
    // flight, so from here the generation record and executable stand witness.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 9001,
        "method": "tools/call",
        "params": {"name": "orbit_task_list", "arguments": {}},
    }));

    let (mut dashboard, port) = spawn_dashboard(&workspace, &installed);
    let drain = start_drain(&workspace, &installed);
    assert_refused(&local_update());
    assert_eq!(generation_record(&workspace), record);
    assert_eq!(
        executable_generation(&installed).expect("installed"),
        old_digest
    );
    assert!(!install.join("orbit.previous").exists());

    // Every client is still served by the build it started on.
    let reply = loop {
        let line = client
            .lines
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("the unread reply");
        let message: Value = serde_json::from_str(line.trim()).expect("JSON-RPC");
        if message["id"] == 9001 {
            break message;
        }
    };
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    let task = client.call_tool_ok(
        "orbit_task_add",
        json!({"title":"Written beside a refused local candidate", "description":"Same live authority", "complexity":"low", "model":"codex"}),
    );
    assert_eq!(task["title"], "Written beside a refused local candidate");
    assert!(http_get(port, "/healthz").contains("ok"));
    let run = run_show(&workspace, &drain.run_id);
    assert_eq!(run["run"]["state"], "running", "{run}");
    assert_eq!(run["run"]["pid"].as_u64(), Some(u64::from(drain.pid)));
    assert_eq!(
        running_digest(&workspace, drain.pid).as_deref(),
        Some(old_digest.as_str())
    );

    drop(client);
    stop(&mut dashboard);
    cancel_drain(&workspace, &drain);
    let output = local_update();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).expect("update report");
    assert_eq!(report["outcome"], "updated", "{report}");
    assert_eq!(
        report["local_candidate"]["source_commit"]["value"],
        CANDIDATE_COMMIT
    );
    assert_eq!(
        std::fs::read(&installed).expect("installed"),
        std::fs::read(&candidate).expect("candidate")
    );

    // Each client kind reconnects to the installed candidate.
    let mut client = serve(&installed);
    let tasks = client.call_tool_ok("orbit_task_list", json!({}));
    assert_eq!(tasks["total"], 1, "{tasks}");
    let (mut dashboard, port) = spawn_dashboard(&workspace, &installed);
    assert!(http_get(port, "/healthz").contains("ok"));
    candidate_ok(&workspace, &installed, &["clock", "tick"]);
    let drain = start_drain(&workspace, &installed);
    assert_eq!(
        running_digest(&workspace, drain.pid),
        Some(executable_generation(&candidate).expect("candidate digest"))
    );
    drop(client);
    stop(&mut dashboard);
    cancel_drain(&workspace, &drain);
}
