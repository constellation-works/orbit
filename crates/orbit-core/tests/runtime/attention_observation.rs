//! Attention is observational: a pending task-commit journal must survive
//! an `orbit.attention` query untouched and be reported as degraded, while
//! an ordinary operational metadata read still recovers it.

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRunState, JobTargetType};
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

fn friction_attention_does_not_recover_a_pending_task_commit() {
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
            title: "Pending commit friction fixture".into(),
            description: "Fixture task.".into(),
            acceptance_criteria: vec!["Fixture task is observable.".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    let workspace_id = runtime.workspace_id().unwrap();

    use orbit_store::contracts::FrictionAddParams;
    let store = orbit_core::runtime::friction::store_for(&runtime).unwrap();
    let open = store
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Open friction".into()),
            body: "Open friction body".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Open,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    store
        .add(FrictionAddParams {
            model: "test".into(),
            title: Some("Triaged friction".into()),
            body: "Triaged body".into(),
            tags: vec!["other".into()],
            status: orbit_types::record::FrictionStatus::Triaged,
            during_task: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

    {
        let connection = rusqlite::Connection::open(global.join("orbit.db")).unwrap();
        connection
            .execute(
                "INSERT INTO task_commit_journal \
                     (journal_id, workspace_id, task_id, state, intent_json, created_at) \
                 VALUES (?1, ?2, ?3, 'prepared', '{}', ?4)",
                rusqlite::params![
                    "attention-pending-friction-1",
                    workspace_id,
                    task.id,
                    chrono::Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
    }
    assert_eq!(
        journal_state(&runtime, "attention-pending-friction-1").as_deref(),
        Some("prepared")
    );
    let before = runtime.observe_task_metadata().unwrap();
    assert!(runtime.inspect_execution_claims().is_err());

    // Friction reads use a separate workspace-partitioned store, so a
    // friction-only query must return the open record (never an empty
    // all-clear) without settling the pending task journal.
    let response = call(&runtime, json!({"kind": "friction_untriaged"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["kind"], "friction_untriaged", "{response}");
    assert_eq!(
        items[0]["refs"]["friction_id"], open.record.id,
        "{response}"
    );
    assert_eq!(
        journal_state(&runtime, "attention-pending-friction-1").as_deref(),
        Some("prepared"),
        "friction attention must not abort or replay the pending commit: {response}"
    );
    assert_eq!(runtime.observe_task_metadata().unwrap(), before);
    assert!(
        runtime.inspect_execution_claims().is_err(),
        "attention left the pending commit for an ordinary read"
    );

    // Once an ordinary read settles the journal, settled friction status
    // filtering continues to list only the open record.
    runtime.list_task_metadata().unwrap();
    assert_eq!(
        journal_state(&runtime, "attention-pending-friction-1").as_deref(),
        Some("aborted")
    );
    let response = call(&runtime, json!({"kind": "friction_untriaged"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(
        items[0]["refs"]["friction_id"], open.record.id,
        "{response}"
    );
}

fn delivery_hold_attention_next_is_workspace_qualified_with_tool() {
    for (code, expected_action, expected_verb) in [
        (
            "delivery_forge_observation_unavailable",
            "resume",
            "job resume",
        ),
        ("delivery_pr_draft", "show", "run show"),
    ] {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let orbit = root.path().join("repo/.orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&orbit).unwrap();
        std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
        let operator = session(McpCapability::Operator);
        let workspace_id = runtime.workspace_id().unwrap();

        let task = runtime
            .add_task(TaskAddParams {
                title: "Delivery hold attention fixture".into(),
                description: "Fixture task.".into(),
                acceptance_criteria: vec!["Fixture task is observable.".into()],
                status: Some(TaskStatus::Review),
                ..Default::default()
            })
            .unwrap();

        let jobs = runtime.stores().jobs();
        let now = Utc::now();
        let run = jobs
            .insert_job_run(
                "task_pr_pipeline",
                1,
                now,
                Some(json!({"task_ids": [task.id]})),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, now, std::process::id())
            .unwrap();
        jobs.complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "complete".into(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: None,
                agent_response_json: None,
                state: JobRunState::Held,
                error_code: Some(code.into()),
                error_message: Some("held".into()),
            },
        )
        .unwrap();
        jobs.finalize_job_run(&run.run_id, JobRunState::Held, now, None)
            .unwrap();

        let response = call(&runtime, json!({"kind": "review_waiting"}), &operator);
        let items = response["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{code}: {response}");
        let item = &items[0];
        assert_eq!(item["kind"], "review_waiting", "{code}: {response}");
        assert_eq!(
            item["detail"]["waiting_on"], "delivery_hold",
            "{code}: {response}"
        );
        assert_eq!(item["detail"]["code"], code, "{code}: {response}");
        assert_eq!(item["detail"]["run_id"], run.run_id, "{code}: {response}");
        assert_eq!(
            item["next"]["command"],
            format!(
                "orbit --workspace '{workspace_id}' {expected_verb} {}",
                run.run_id
            ),
            "{code}: next.command must carry --workspace"
        );
        assert_eq!(
            item["next"]["tool"], "orbit.run",
            "{code}: next.tool must name the MCP tool"
        );
        assert_eq!(
            item["next"]["input"],
            json!({"action": expected_action, "id": run.run_id, "workspace": workspace_id}),
            "{code}: next.input must carry run id and workspace"
        );
    }
}

crate::scenario_table!(
    attention_observation,
    attention_does_not_recover_a_pending_task_commit,
    friction_attention_does_not_recover_a_pending_task_commit,
    delivery_hold_attention_next_is_workspace_qualified_with_tool,
);
