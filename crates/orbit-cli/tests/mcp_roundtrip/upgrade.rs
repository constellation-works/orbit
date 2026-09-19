//! Real process coverage: no replacement server, retries or substitute authority.
use super::*;
use orbit_common::fs::generation::executable_generation;

fn preflight(workspace: &McpWorkspace) -> std::process::Output {
    McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["update", "--preflight", "--json"])
        .output()
        .expect("preflight")
}

fn assert_refused(output: &std::process::Output) {
    assert!(!output.status.success(), "unexpected success: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("upgrade admission refused"),
        "{output:?}"
    );
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
fn persistent_client_upgrade_refusal_preserves_inode_schema_and_audited_calls() {
    let workspace = McpWorkspace::init();
    let install = workspace.home.join("installation");
    std::fs::create_dir_all(&install).expect("installation");
    let old = install.join("orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &old).expect("copy installed executable");
    let old_digest = executable_generation(&old).expect("old digest");
    let child = McpWorkspace::orbit_program_command(&old, &workspace.work, &workspace.home)
        .args([
            "mcp",
            "serve",
            "--operator",
            "--workspace",
            "ws_mcp-roundtrip",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("old server");
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
    let before = store_bytes(&workspace);
    let contract = McpWorkspace::orbit_command(&workspace.work, &workspace.home)
        .args(["update", "--contract", "--json"])
        .output()
        .expect("contract probe");
    assert!(contract.status.success(), "{contract:?}");
    let report: Value = serde_json::from_slice(&contract.stdout).expect("contract JSON");
    assert_eq!(report["contract"], "executable-generation-v1");
    assert_refused(&preflight(&workspace));
    // Exercise ordinary update admission, not just the observation helper. An
    // explicit target avoids network access; refusal must precede staging.
    let output = McpWorkspace::orbit_program_command(&old, &workspace.work, &workspace.home)
        .env("ORBIT_INSTALL_DIR", &install)
        .args(["update", "--version", "99.0.0", "--json"])
        .output()
        .expect("attempt update");
    assert_refused(&output);
    assert_eq!(
        store_bytes(&workspace),
        before,
        "refusal touched store/layout bytes"
    );
    assert_eq!(
        executable_generation(&old).expect("installed digest"),
        old_digest
    );
    assert!(!install.join("orbit.previous").exists());
    assert_eq!(client.child.id(), pid);
    #[cfg(target_os = "linux")]
    assert_eq!(
        executable_generation(&PathBuf::from(format!("/proc/{pid}/exe"))).expect("running inode"),
        old_digest
    );
    client.call_tool_ok("orbit_workspace_list", json!({}));
    let task = client.call_tool_ok("orbit_task_add", json!({"title":"After refused upgrade", "description":"Same live authority", "complexity":"low", "model":"codex"}));
    assert_eq!(task["title"], "After refused upgrade");
    assert_eq!(audit_count(&workspace, "orbit.task.add"), 1);
    assert_eq!(audit_count(&workspace, "orbit.workspace.list"), 2);
    drop(client);
    let ready = preflight(&workspace);
    assert!(ready.status.success(), "{ready:?}");
    let report: Value = serde_json::from_slice(&ready.stdout).expect("preflight JSON");
    assert_eq!(report["reservation"], false);
}

#[test]
#[cfg(target_os = "linux")]
fn different_executable_cannot_auto_migrate_while_old_client_is_live() {
    let workspace = McpWorkspace::init();
    let mut client = workspace.serve();
    client.call_tool_ok("orbit_workspace_list", json!({}));
    // An executable with distinct bytes but the same version and schema proves
    // admission is conservative; neither version equality nor additive shape
    // may grant an old writer a waiver.
    let candidate = workspace.home.join("candidate-orbit");
    std::fs::copy(env!("CARGO_BIN_EXE_orbit"), &candidate).expect("candidate copy");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&candidate)
        .expect("open candidate")
        .write_all(b"\nupgrade-regression-candidate\n")
        .expect("distinct executable");
    assert_ne!(
        executable_generation(&candidate).expect("candidate"),
        executable_generation(Path::new(env!("CARGO_BIN_EXE_orbit"))).expect("old")
    );
    let before = store_bytes(&workspace);
    for args in [
        vec!["migrate", "--confirm"],
        vec!["workspace", "sync"],
        vec!["task", "list", "--json"],
        vec!["mcp", "serve"],
    ] {
        let output =
            McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .expect("candidate launch");
        assert_refused(&output);
        assert_eq!(store_bytes(&workspace), before);
    }
    client.call_tool_ok("orbit_workspace_list", json!({}));
    drop(client);
    let migrated =
        McpWorkspace::orbit_program_command(&candidate, &workspace.work, &workspace.home)
            .args(["migrate", "--confirm"])
            .output()
            .expect("candidate after quiescence");
    assert!(migrated.status.success(), "{migrated:?}");
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
    client.call_tool_ok("orbit_workspace_list", json!({}));
    drop(client);
    assert!(preflight(&workspace).status.success());
}
