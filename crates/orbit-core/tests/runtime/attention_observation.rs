//! Attention is observational: a pending task-commit journal must survive
//! an `orbit.attention` query untouched and be reported as degraded, while
//! an ordinary operational metadata read still recovers it.

use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

fn session(capability: McpCapability) -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: [capability].into(),
        ..Default::default()
    }
}

fn call(runtime: &OrbitRuntime, input: Value, session: &ToolSessionContext) -> Value {
    runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            input,
            None,
            None,
            ToolEntryPoint::Mcp,
            session.clone(),
        )
        .map(|outcome| outcome.value)
        .unwrap_or_else(|error| panic!("attention call failed: {error}"))
}

fn journal_state(runtime: &OrbitRuntime, journal_id: &str) -> Option<String> {
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection
        .query_row(
            "SELECT state FROM task_commit_journal WHERE journal_id = ?1",
            [journal_id],
            |row| row.get(0),
        )
        .ok()
}

fn degraded_kinds(response: &Value) -> Vec<String> {
    response["degraded"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn attention_does_not_recover_a_pending_task_commit() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    let operator = session(McpCapability::Operator);

    let task = runtime
        .add_task(TaskAddParams {
            title: "Pending commit attention fixture".into(),
            description: "Fixture task.".into(),
            acceptance_criteria: vec!["Fixture task is observable.".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let workspace_id = runtime.workspace_id().unwrap();

    // A committed-transition committer that died before applying leaves an
    // unsettled journal row nobody holds a lease for. A `prepared` row is
    // the same pending state recovery would abort on an ordinary read.
    {
        let connection = rusqlite::Connection::open(global.join("orbit.db")).unwrap();
        connection
            .execute(
                "INSERT INTO task_commit_journal \
                     (journal_id, workspace_id, task_id, state, intent_json, created_at) \
                 VALUES (?1, ?2, ?3, 'prepared', '{}', ?4)",
                rusqlite::params![
                    "attention-pending-1",
                    workspace_id,
                    task.id,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }
    assert_eq!(
        journal_state(&runtime, "attention-pending-1").as_deref(),
        Some("prepared")
    );
    let before = runtime.observe_task_metadata().unwrap();
    assert!(
        runtime.inspect_execution_claims().is_err(),
        "the pending journal makes claim inspection unavailable"
    );

    // Runtime-only selection: degraded, and the journal/task state is left
    // exactly as the interrupted commit left it.
    for kind in ["claim_stale", "run_failed", "run_stuck"] {
        let response = call(&runtime, json!({"kind": kind}), &operator);
        assert!(
            degraded_kinds(&response)
                .iter()
                .any(|degraded| degraded == kind),
            "{kind} must degrade while a commit is pending: {response}"
        );
        assert_eq!(
            journal_state(&runtime, "attention-pending-1").as_deref(),
            Some("prepared"),
            "{kind} attention must not abort or replay the pending commit"
        );
    }

    // Default/task-level selection preserves the same guarantee.
    let response = call(&runtime, json!({}), &operator);
    assert!(
        degraded_kinds(&response)
            .iter()
            .any(|kind| kind == "pilot_hold"),
        "default attention must degrade task-level sources: {response}"
    );
    assert_eq!(
        journal_state(&runtime, "attention-pending-1").as_deref(),
        Some("prepared"),
        "default attention must not recover the pending commit"
    );
    assert_eq!(runtime.observe_task_metadata().unwrap(), before);
    assert!(
        runtime.inspect_execution_claims().is_err(),
        "attention left the pending commit for an ordinary read"
    );

    // Ordinary operational reads retain recovery behavior.
    runtime.list_task_metadata().unwrap();
    assert_eq!(
        journal_state(&runtime, "attention-pending-1").as_deref(),
        Some("aborted"),
        "an ordinary metadata read aborts the prepared journal row"
    );
    assert!(runtime.inspect_execution_claims().is_ok());
}

crate::scenario_table!(
    attention_observation,
    attention_does_not_recover_a_pending_task_commit,
);
