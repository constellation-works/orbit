//! Attention is observational: a pending task-commit journal must survive
//! an `orbit.attention` query untouched and be reported as degraded, while
//! an ordinary operational metadata read still recovers it.

use chrono::{Duration, SecondsFormat, Utc};
use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::runtime::OrbitRuntime;
use orbit_store::contract::{
    AdmissionRunContext, ClaimInspection, ClaimRun, ExecutionClaim, ExecutionClaimPhase,
    ExecutionLocation, JobRunStepParams, LocalPullAdmission, LocalPullPhase, PullDestination,
};
use orbit_types::task::TaskStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::PipelineState;
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::{Value, json};

const ATTENTION_HOST: &str = "attention-host";

const CONFIG: &str =
    "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n";

struct SetupRun<'a> {
    job_id: &'a str,
    input: Value,
    run_started_at: chrono::DateTime<Utc>,
    setup_started_at: chrono::DateTime<Utc>,
    setup_finished_at: chrono::DateTime<Utc>,
    run_finished_at: chrono::DateTime<Utc>,
    state: JobRunState,
    error: Option<String>,
}

impl<'a> SetupRun<'a> {
    fn at(
        job_id: &'a str,
        input: Value,
        at: chrono::DateTime<Utc>,
        state: JobRunState,
        error: Option<String>,
    ) -> Self {
        Self {
            job_id,
            input,
            run_started_at: at,
            setup_started_at: at,
            setup_finished_at: at,
            run_finished_at: at,
            state,
            error,
        }
    }
}

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

fn pilot_warning_attention_preserves_a_commit_interrupted_during_traversal() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    struct InterruptOnPrerequisiteRead<F> {
        interrupted: AtomicBool,
        interrupt: F,
    }

    impl<S, F> tracing_subscriber::Layer<S> for InterruptOnPrerequisiteRead<F>
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
        F: Fn() + Send + Sync + 'static,
    {
        fn on_enter(&self, id: &tracing::Id, context: Context<'_, S>) {
            if context
                .span(id)
                .is_some_and(|span| span.name() == "task_bundle_canonical_read")
                && !self.interrupted.swap(true, Ordering::Relaxed)
            {
                (self.interrupt)();
            }
        }
    }

    let (_root, runtime) = attention_runtime();
    let prerequisite = add_named_task(&runtime, "Warning prerequisite", TaskStatus::Backlog);
    let direct = runtime
        .add_task(TaskAddParams {
            title: "Direct prerequisite".into(),
            status: Some(TaskStatus::Backlog),
            dependencies: vec![prerequisite.clone()],
            ..Default::default()
        })
        .unwrap();
    let proposal = runtime
        .add_task(TaskAddParams {
            title: "Proposed warning subject".into(),
            status: Some(TaskStatus::Proposed),
            dependencies: vec![direct.id],
            ..Default::default()
        })
        .unwrap();
    runtime
        .update_task_as_human(
            &proposal.id,
            TaskUpdateParams {
                comment: Some(format!(
                    "operation_id={}\n{}",
                    "a".repeat(64),
                    json!({"assessment": {"blocked_by": ["Confirm the operator workflow"]}}),
                )),
                ..Default::default()
            },
            "task-pilot".into(),
        )
        .unwrap();

    use orbit_core::test_support::fixture_store::maintenance::task_registry::{
        TaskRegistryStore, task_registry_path,
    };
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let binding = registry.find_task_binding(&prerequisite).unwrap().unwrap();
    let lease = binding
        .canonical_path
        .parent()
        .unwrap()
        .join(".task-commit-applying")
        .join(&prerequisite);
    std::fs::create_dir_all(lease.parent().unwrap()).unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let database = runtime.global_root().join("orbit.db");
    let journal = "attention-pilot-warning";
    let interrupt = {
        let lease = lease.clone();
        let task_id = prerequisite.clone();
        move || {
            // The direct prerequisite is already being read. Interrupt the
            // transitive prerequisite before the walk reaches it.
            std::fs::File::create(&lease).unwrap();
            rusqlite::Connection::open(&database)
                .unwrap()
                .execute(
                    "INSERT INTO task_commit_journal \
                     (journal_id, workspace_id, task_id, state, intent_json, created_at) \
                     VALUES (?1, ?2, ?3, 'prepared', '{}', ?4)",
                    rusqlite::params![journal, workspace_id, task_id, Utc::now().to_rfc3339()],
                )
                .unwrap();
        }
    };
    let subscriber = tracing_subscriber::registry().with(InterruptOnPrerequisiteRead {
        interrupted: AtomicBool::new(false),
        interrupt,
    });
    let operator = session(McpCapability::Operator);
    let query = || call(&runtime, json!({"kind": "pilot_warning"}), &operator);
    let response = tracing::subscriber::with_default(subscriber, query);
    assert_eq!(
        journal_state(&runtime, journal).as_deref(),
        Some("prepared"),
        "{response}"
    );
    assert!(lease.exists());
    assert_eq!(response["items"], json!([]));
    let degraded = response["degraded"].as_array().unwrap();
    assert_eq!(degraded.len(), 1, "{response}");
    assert_eq!(degraded[0]["kind"], "pilot_warning");
    assert_eq!(degraded[0]["code"], "source_unavailable");
    assert!(
        degraded[0]["message"]
            .as_str()
            .unwrap()
            .contains(&prerequisite)
    );

    runtime.get_task(&proposal.id).unwrap();
    assert_eq!(journal_state(&runtime, journal).as_deref(), Some("aborted"));
    assert!(!lease.exists());
    let response = query();
    assert_eq!(response["degraded"], json!([]));
    assert_eq!(response["items"][0]["refs"]["task_id"], proposal.id);
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

    use orbit_store::contract::FrictionAddParams;
    let store = orbit_core::runtime::stored::friction::store_for(&runtime).unwrap();
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
        ("delivery_repair_dispatched", "show", "run show"),
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
        if code == "delivery_repair_dispatched" {
            let summary = item["next"]["summary"].as_str().unwrap();
            // A spent no-edit rerun holds again on the same head even with repair budget.
            assert!(
                summary.contains("inspect the hold reason"),
                "ORB-27293: {response}"
            );
            assert!(
                summary.contains("unchanged-head rerun is spent"),
                "ORB-27293: {response}"
            );
            assert!(
                summary.contains("repair and publish a new head"),
                "ORB-27293: {response}"
            );
            assert!(
                summary.contains("no-edit rerun is not spent"),
                "ORB-27293: {response}"
            );
            assert!(
                summary.contains("remaining per-PR repair budget on the same head"),
                "ORB-27293: {response}"
            );
        }

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

fn run_stuck_respects_declared_windows_and_progress() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit).unwrap();
    let operator = session(McpCapability::Operator);
    let jobs = runtime.stores().jobs();
    let now = Utc::now();

    // Seed a running run started `started` ago. When `deadline` is set the
    // pipeline state stamps it as `open_window` (as `drain_window` does)
    // and `progress` becomes the state's `updated_at`.
    let seed = |job: &str,
                input: Value,
                started: chrono::DateTime<Utc>,
                deadline: Option<chrono::DateTime<Utc>>,
                progress: Option<chrono::DateTime<Utc>>|
     -> String {
        let run = jobs
            .insert_job_run(job, 1, started, Some(input.clone()), None)
            .unwrap();
        jobs.mark_job_run_running(&run.run_id, started, std::process::id())
            .unwrap();
        if let Some(deadline) = deadline {
            let mut state = PipelineState::new(run.run_id.clone(), job.to_string(), input);
            state.record_pipeline_output(
                "open_window",
                json!({"deadline": deadline.to_rfc3339_opts(SecondsFormat::Secs, true)}),
            );
            state.updated_at = progress.unwrap_or(now);
            jobs.write_run_state(&run.run_id, &state).unwrap();
        }
        run.run_id
    };

    // Long drain inside its 5h window, pipeline advancing now: healthy.
    let healthy = seed(
        "workspace_auto_pipeline",
        json!({"for_seconds": 18000}),
        now - Duration::hours(3),
        Some(now + Duration::hours(2)),
        Some(now),
    );
    // Drain 5h past its 1h deadline (beyond the grace bound), even though
    // its pipeline still advances: stuck.
    let past_deadline = seed(
        "workspace_auto_pipeline",
        json!({"for_seconds": 3600}),
        now - Duration::hours(6),
        Some(now - Duration::hours(5)),
        Some(now),
    );
    // Drain inside its window whose progress stalled at its start: stuck.
    let stalled = seed(
        "workspace_auto_pipeline",
        json!({"for_seconds": 18000}),
        now - Duration::hours(3),
        Some(now + Duration::hours(2)),
        Some(now - Duration::hours(3)),
    );
    // No declared bound, running for 3h: fixed fallback still flags it.
    let boundless = seed("some_job", json!({}), now - Duration::hours(3), None, None);

    let response = call(&runtime, json!({"kind": "run_stuck"}), &operator);
    let flagged: Vec<&str> = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["refs"]["run_id"].as_str().unwrap())
        .collect();
    assert!(
        !flagged.contains(&healthy.as_str()),
        "drain inside its window with advancing pipeline must not be run_stuck: {response}"
    );
    for expected in [&past_deadline, &stalled, &boundless] {
        assert!(
            flagged.contains(&expected.as_str()),
            "run {expected} must be run_stuck: {response}"
        );
    }
    let past = response["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["refs"]["run_id"] == past_deadline)
        .unwrap();
    assert!(
        past["detail"]["declared_deadline"].is_string(),
        "past-deadline item names its declared deadline: {response}"
    );
}

/// A stuck run's item names the run's latest recorded step, so a caller
/// without the operator-only run read can tell an active run from an
/// abandoned one. A run with no recorded step reports null.
fn run_stuck_names_the_current_step() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let jobs = runtime.stores().jobs();
    let now = Utc::now();
    let started = now - Duration::hours(3);

    let record_step = |run_id: &str, index: usize, target: &str, state: JobRunState| {
        jobs.complete_job_run_step(
            run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: index,
                target_type: JobTargetType::Activity,
                target_id: target.into(),
                started_at: started,
                finished_at: started,
                duration_ms: Some(1),
                exit_code: None,
                agent_response_json: Some(json!({"log": "never copied into attention"})),
                state,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
    };

    // Over its bound with the live owner (this process) and two steps.
    let slow = jobs
        .insert_job_run("some_job", 1, started, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&slow.run_id, started, std::process::id())
        .unwrap();
    record_step(&slow.run_id, 0, "worktree_setup", JobRunState::Success);
    record_step(&slow.run_id, 1, "implement", JobRunState::Success);

    // An owner that is gone, with one recorded step, and another with none.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let dead = jobs
        .insert_job_run("some_job", 1, started, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&dead.run_id, started, dead_pid)
        .unwrap();
    record_step(&dead.run_id, 0, "worktree_setup", JobRunState::Failed);
    let fresh = jobs
        .insert_job_run("some_job", 1, started, Some(json!({})), None)
        .unwrap();
    jobs.mark_job_run_running(&fresh.run_id, started, dead_pid)
        .unwrap();

    let response = call(&runtime, json!({"kind": "run_stuck"}), &operator);
    let item = |run_id: &str| -> Value {
        response["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["refs"]["run_id"] == run_id)
            .unwrap_or_else(|| panic!("run {run_id} must be run_stuck: {response}"))
            .clone()
    };

    let slow_item = item(&slow.run_id);
    assert_eq!(slow_item["detail"]["cause"], "wall_clock_bound");
    assert_eq!(slow_item["detail"]["current_step_id"], "implement");
    assert_eq!(slow_item["detail"]["current_step_status"], "success");
    assert!(slow_item["detail"]["last_progress_at"].is_string());
    assert!(
        !slow_item
            .to_string()
            .contains("never copied into attention"),
        "step output stays out of attention: {slow_item}"
    );

    let dead_item = item(&dead.run_id);
    assert_eq!(dead_item["detail"]["cause"], "dead_owner");
    assert_eq!(dead_item["detail"]["current_step_id"], "worktree_setup");
    assert_eq!(dead_item["detail"]["current_step_status"], "failed");

    let fresh_item = item(&fresh.run_id);
    assert_eq!(fresh_item["detail"]["cause"], "dead_owner");
    assert!(fresh_item["detail"]["current_step_id"].is_null());
    assert!(fresh_item["detail"]["current_step_status"].is_null());
}

fn attention_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let orbit = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&orbit).unwrap();
    std::fs::write(orbit.join("config.toml"), CONFIG).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &orbit)
        .unwrap()
        .with_automation_machine_identity(Some(ATTENTION_HOST.into()));
    (root, runtime)
}

fn add_named_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "Fixture task.".into(),
            acceptance_criteria: vec!["Fixture task is observable.".into()],
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn settle_task(runtime: &OrbitRuntime, id: &str) {
    runtime
        .update_task_with_identity(
            id,
            TaskUpdateParams {
                status: Some(TaskStatus::Done),
                execution_summary: Some("Landed through a later run.".into()),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
}

fn fail_run(runtime: &OrbitRuntime, task_id: Option<&str>) -> String {
    let jobs = runtime.stores().jobs();
    let now = Utc::now();
    let input = task_id.map(|id| json!({"task_id": id}));
    let run = jobs
        .insert_job_run("task_pr_pipeline", 1, now, input, None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, now, None)
        .unwrap();
    run.run_id
}

fn run_item<'a>(response: &'a Value, run_id: &str) -> &'a Value {
    response["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["refs"]["run_id"] == run_id)
        .unwrap_or_else(|| panic!("missing run {run_id} in {response}"))
}

fn assert_run_failed_claim(item: &Value, held: bool, leaf: bool, severity: &str) {
    assert_eq!(item["kind"], "run_failed", "{item}");
    assert_eq!(item["detail"]["held_claim"], json!(held), "{item}");
    assert_eq!(item["detail"]["claimed_leaf"], json!(leaf), "{item}");
    assert_eq!(item["severity"], severity, "{item}");
    if leaf {
        // Resume advice stays on the claimed-leaf flag, including after settlement.
        assert_eq!(item["next"]["input"]["action"], "show", "{item}");
    }
}

fn run_failed_degraded(response: &Value) -> Vec<&Value> {
    response["degraded"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "run_failed")
        .collect()
}

fn owner_run_failed_skips_a_terminal_task_and_keeps_an_open_one() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let done = add_named_task(&runtime, "Settled owner task", TaskStatus::Review);
    settle_task(&runtime, &done);
    let open = add_named_task(&runtime, "Open owner task", TaskStatus::Backlog);
    let done_run = fail_run(&runtime, Some(&done));
    let open_run = fail_run(&runtime, Some(&open));

    let response = call(&runtime, json!({"kind": "run_failed"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert!(
        !items.iter().any(|item| item["refs"]["run_id"] == done_run),
        "a terminal owner task suppresses its failed run: {response}"
    );
    assert_eq!(items.len(), 1, "{response}");
    let item = run_item(&response, &open_run);
    assert_eq!(item["refs"]["task_id"], open, "{response}");
    assert_run_failed_claim(item, false, false, "medium");
    assert!(
        run_failed_degraded(&response).is_empty(),
        "the owner can read task status, so run_failed is not degraded: {response}"
    );
}

fn follower_run_failed_degrades_when_the_owner_task_status_cannot_be_read() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let done = add_named_task(&runtime, "Done on the shared store", TaskStatus::Review);
    settle_task(&runtime, &done);
    let open = add_named_task(&runtime, "Open on the shared store", TaskStatus::Backlog);
    let done_run = fail_run(&runtime, Some(&done));
    let open_run = fail_run(&runtime, Some(&open));
    let replica = runtime
        .clone()
        .with_coordination_write_owner(Some("hm_owner".into()));

    let response = call(&replica, json!({"kind": "run_failed"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{response}");
    let settled = run_item(&response, &done_run);
    assert_eq!(settled["refs"]["task_id"], done, "{response}");
    assert_run_failed_claim(settled, false, false, "medium");
    assert_run_failed_claim(run_item(&response, &open_run), false, false, "medium");
    let degraded = run_failed_degraded(&response);
    assert_eq!(degraded.len(), 1, "{response}");
    assert_eq!(degraded[0]["code"], "source_unavailable", "{response}");
    assert_eq!(
        degraded[0]["message"],
        "Task records on a replica are not authoritative; read the workspace owner",
        "{response}"
    );

    // A failed run that names no task does not, by itself, degrade the kind.
    let (_taskless_root, taskless) = attention_runtime();
    let taskless = taskless.with_coordination_write_owner(Some("hm_owner".into()));
    let bare = fail_run(&taskless, None);
    let response = call(&taskless, json!({"kind": "run_failed"}), &operator);
    let item = run_item(&response, &bare);
    assert!(!item["refs"]["task_id"].is_string(), "{response}");
    assert_run_failed_claim(item, false, false, "medium");
    assert!(
        run_failed_degraded(&response).is_empty(),
        "a taskless follower failure is not an owner-status gap: {response}"
    );
}

fn watcher_supersession_requires_a_newer_verified_delivery() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let jobs = runtime.stores().jobs();
    let now = Utc::now();
    for (title, newer, delivery, matches_task) in [
        ("Replaced", true, true, true),
        ("Current parent", false, true, true),
        ("Different job", true, false, true),
        ("Different task", true, true, false),
    ] {
        let task = add_named_task(&runtime, title, TaskStatus::Backlog);
        let watcher = jobs
            .insert_job_run(
                "task_pr_watch_pipeline",
                1,
                now,
                Some(json!({"task_ids": [task]})),
                None,
            )
            .unwrap();
        jobs.mark_job_run_running(&watcher.run_id, now, std::process::id())
            .unwrap();
        jobs.finalize_job_run(&watcher.run_id, JobRunState::Failed, now, None)
            .unwrap();
        let before = call(&runtime, json!({"kind": "run_failed"}), &operator);
        let original_next = run_item(&before, &watcher.run_id)["next"].clone();
        let owner = jobs
            .insert_job_run(
                if delivery {
                    "task_pr_pipeline"
                } else {
                    "other_job"
                },
                1,
                now,
                Some(json!({"task_ids": [if matches_task { task.as_str() } else { "other" }]})),
                None,
            )
            .unwrap();
        // Explicit timestamps make ordering independent of clock resolution.
        let connection =
            rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
        let owner_time = if newer {
            now + Duration::minutes(1)
        } else {
            now - Duration::minutes(1)
        };
        connection
            .execute(
                "UPDATE job_runs SET created_at = ?1 WHERE run_id = ?2",
                rusqlite::params![owner_time.to_rfc3339(), owner.run_id],
            )
            .unwrap();
        runtime
            .update_task_with_identity(
                &task,
                TaskUpdateParams {
                    job_run_id: Some(Some(owner.run_id)),
                    ..Default::default()
                },
                Some("codex".into()),
                None,
            )
            .unwrap();
        let response = call(&runtime, json!({"kind": "run_failed"}), &operator);
        let item = run_item(&response, &watcher.run_id);
        if newer && delivery && matches_task {
            assert_eq!(item["detail"]["superseded"], true, "{item}");
            assert_eq!(item["next"]["tool"], "orbit.task.show", "{item}");
            assert_eq!(item["next"]["input"]["id"], task, "{item}");
            assert!(!item["next"].to_string().contains("resume"), "{item}");
        } else {
            assert!(item["detail"]["superseded"].is_null(), "{item}");
            assert_eq!(item["next"], original_next, "{item}");
        }
    }
}

pub(crate) fn seed_bound_claim(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    phase: ExecutionClaimPhase,
    machine_id: &str,
) {
    let now = Utc::now().to_rfc3339();
    let claim = ExecutionClaim {
        claim_id: format!("claim-{run_id}"),
        task_id: task_id.to_string(),
        request_id: format!("request-{run_id}"),
        executed_on: ExecutionLocation {
            machine_id: machine_id.to_string(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: format!("parent-{run_id}"),
            job_name: "workspace_auto_pipeline".into(),
            machine_name: None,
        },
        footprint: Vec::new(),
        reservation_id: format!("reservation-{run_id}"),
        reservation_expires_at: (Utc::now() + Duration::minutes(30)).to_rfc3339(),
        phase,
        repair: None,
    };
    let inspection = ClaimInspection {
        claim: claim.clone(),
        bound_run: Some(ClaimRun {
            machine_id: machine_id.to_string(),
            run_id: run_id.to_string(),
        }),
        created_at: now.clone(),
        updated_at: now.clone(),
        last_event: "claim_failed".into(),
        age_seconds: Some(1),
        unresolved_merge_intent: None,
        landing_invalidated: false,
        release: None,
        preserved_candidate: None,
        settlement: None,
        steering: None,
        leaf_run_evidence: None,
        late_leaf_effort: None,
        late_leaf_model: None,
    };
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    for (kind, payload) in [
        (
            "distributed-execution-claim-v1",
            serde_json::to_string(&claim).unwrap(),
        ),
        (
            "distributed-claim-lifecycle-v1",
            serde_json::to_string(&inspection).unwrap(),
        ),
    ] {
        connection
            .execute(
                "INSERT INTO task_coordination_rows \
                     (workspace_id, kind, row_id, payload_json, journal_id, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'fixture', ?5)",
                rusqlite::params![workspace_id, kind, claim.claim_id, payload, now],
            )
            .unwrap();
    }
}

fn run_failed_settled_claims_are_not_held_and_a_live_claim_is() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let mut seeded = Vec::new();
    for (title, phase, held, severity) in [
        ("Failed claim", ExecutionClaimPhase::Failed, false, "medium"),
        ("Landed claim", ExecutionClaimPhase::Landed, false, "medium"),
        (
            "Revoked claim",
            ExecutionClaimPhase::Revoked,
            false,
            "medium",
        ),
        ("Running claim", ExecutionClaimPhase::Running, true, "high"),
    ] {
        let task = add_named_task(&runtime, title, TaskStatus::Backlog);
        let run_id = fail_run(&runtime, Some(&task));
        seed_bound_claim(&runtime, &task, &run_id, phase, ATTENTION_HOST);
        seeded.push((run_id, held, severity));
    }
    let foreign_task = add_named_task(&runtime, "Other host claim", TaskStatus::Backlog);
    let foreign_run = fail_run(&runtime, Some(&foreign_task));
    seed_bound_claim(
        &runtime,
        &foreign_task,
        &foreign_run,
        ExecutionClaimPhase::Running,
        "other-host",
    );

    let response = call(&runtime, json!({"kind": "run_failed"}), &operator);
    assert_eq!(response["items"].as_array().unwrap().len(), 5, "{response}");
    for (run_id, held, severity) in seeded {
        assert_run_failed_claim(run_item(&response, &run_id), held, true, severity);
    }
    assert_run_failed_claim(run_item(&response, &foreign_run), false, false, "medium");
    assert!(
        run_failed_degraded(&response).is_empty(),
        "owner claim inspection is not a replica status gap: {response}"
    );
}

pub(crate) fn seed_local_pull(
    runtime: &OrbitRuntime,
    run_id: &str,
    phase: LocalPullPhase,
    request_id: &str,
) {
    let admission = LocalPullAdmission {
        destination: PullDestination {
            owner_machine_id: "owner-m".into(),
            owner_workspace_id: "owner-ws".into(),
            selector: "owner-m/owner-ws".into(),
            execution_machine_id: "exec-m".into(),
        },
        request: crate::admission_test_support::admission_request(
            request_id.to_string(),
            AdmissionRunContext {
                run_id: format!("parent-{run_id}"),
                job_name: "workspace_auto_pipeline".into(),
                machine_name: None,
            },
            crate::admission_test_support::admission_ship_contract(),
        ),
        receipt: None,
        leaf_run_id: Some(run_id.to_string()),
        phase,
        settlement: None,
        refusal: None,
        settlement_refusal: None,
        build_output_reclaim_complete: false,
    };
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection
        .execute(
            "INSERT INTO local_pull_admissions (
                workspace_id, owner_machine, owner_workspace, execution_machine,
                request_id, claim_id, leaf_run_id, record_json
             ) VALUES (?1, 'owner-m', 'owner-ws', 'exec-m', ?2, NULL, ?3, ?4)",
            rusqlite::params![
                runtime.workspace_id().unwrap(),
                request_id,
                run_id,
                serde_json::to_string(&admission).unwrap()
            ],
        )
        .unwrap();
}

fn follower_run_failed_settled_local_pull_is_not_a_held_claim() {
    let (_root, runtime) = attention_runtime();
    let operator = session(McpCapability::Operator);
    let settled_task = add_named_task(&runtime, "Settled pull", TaskStatus::Backlog);
    let live_task = add_named_task(&runtime, "Live pull", TaskStatus::Backlog);
    let settled_run = fail_run(&runtime, Some(&settled_task));
    let live_run = fail_run(&runtime, Some(&live_task));
    seed_local_pull(
        &runtime,
        &settled_run,
        LocalPullPhase::Settled,
        "pull-settled",
    );
    seed_local_pull(
        &runtime,
        &live_run,
        LocalPullPhase::Launched,
        "pull-launched",
    );
    let replica = runtime.with_coordination_write_owner(Some("hm_owner".into()));

    let response = call(&replica, json!({"kind": "run_failed"}), &operator);
    assert_eq!(response["items"].as_array().unwrap().len(), 2, "{response}");
    assert_run_failed_claim(run_item(&response, &settled_run), false, true, "medium");
    assert_run_failed_claim(run_item(&response, &live_run), true, true, "high");
    let degraded = run_failed_degraded(&response);
    assert_eq!(
        degraded.len(),
        1,
        "task-naming follower runs still say the owner status was not read: {response}"
    );
    assert_eq!(degraded[0]["code"], "source_unavailable", "{response}");
}

crate::scenario_table!(
    attention_observation,
    attention_does_not_recover_a_pending_task_commit,
    pilot_warning_attention_preserves_a_commit_interrupted_during_traversal,
    friction_attention_does_not_recover_a_pending_task_commit,
    delivery_hold_attention_next_is_workspace_qualified_with_tool,
    run_stuck_respects_declared_windows_and_progress,
    run_stuck_names_the_current_step,
    owner_run_failed_skips_a_terminal_task_and_keeps_an_open_one,
    watcher_supersession_requires_a_newer_verified_delivery,
    follower_run_failed_degrades_when_the_owner_task_status_cannot_be_read,
    run_failed_settled_claims_are_not_held_and_a_live_claim_is,
    follower_run_failed_settled_local_pull_is_not_a_held_claim,
);

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn setup_diverged_owner_base(repo: &std::path::Path) -> (String, String, String) {
    run_git(repo, &["init", "-b", "agent-main"]);
    run_git(repo, &["config", "user.email", "test@example.com"]);
    run_git(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("tracked.txt"), "base").unwrap();
    run_git(repo, &["add", "tracked.txt"]);
    run_git(repo, &["commit", "-m", "base"]);
    let base = run_git(repo, &["rev-parse", "HEAD"]);
    run_git(
        repo,
        &["commit", "--allow-empty", "-m", "local delivery [ORB-123]"],
    );
    let local = run_git(repo, &["rev-parse", "HEAD"]);
    run_git(repo, &["checkout", "--detach", &base]);
    run_git(repo, &["commit", "--allow-empty", "-m", "remote change"]);
    let remote = run_git(repo, &["rev-parse", "HEAD"]);
    run_git(
        repo,
        &["update-ref", "refs/remotes/origin/agent-main", &remote],
    );
    run_git(repo, &["checkout", "agent-main"]);
    (base, local, remote)
}

fn record_setup_run(runtime: &OrbitRuntime, spec: SetupRun<'_>) {
    let jobs = runtime.stores().jobs();
    let run = jobs
        .insert_job_run(spec.job_id, 1, spec.run_started_at, Some(spec.input), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, spec.run_started_at, std::process::id())
        .unwrap();
    jobs.complete_job_run_step(
        &run.run_id,
        &JobRunStepParams {
            lock_wait_ms: None,
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "worktree_setup".into(),
            started_at: spec.setup_started_at,
            finished_at: spec.setup_finished_at,
            duration_ms: Some(1),
            exit_code: None,
            agent_response_json: None,
            state: spec.state,
            error_code: spec
                .error
                .as_ref()
                .map(|_| "delivery_stale_branch".to_string()),
            error_message: spec.error,
        },
    )
    .unwrap();
    jobs.finalize_job_run(&run.run_id, spec.state, spec.run_finished_at, None)
        .unwrap();
}

fn diverged_hold_message(
    repo: &std::path::Path,
    branch: &str,
    local: &str,
    remote: &str,
) -> String {
    format!(
        "worktree setup refusing stale base: owner checkout '{}' holds '{branch}' at {local}, and origin/{branch} is at {remote}; the checkout has diverged",
        repo.display()
    )
}

#[test]
fn stale_owner_base_attention_deduplicates_and_clears() {
    let (root, runtime) = attention_runtime();
    let repo = root.path().join("repo");
    let (base, local, remote) = setup_diverged_owner_base(&repo);
    let now = Utc::now() - Duration::minutes(1);
    let record = |at, state, error: Option<String>| {
        record_setup_run(
            &runtime,
            SetupRun::at(
                "task_local_pipeline",
                json!({"base_branch": "agent-main", "base_sync": "local"}),
                at,
                state,
                error,
            ),
        );
    };
    let message = diverged_hold_message(&repo, "agent-main", &local, &remote);
    record(now, JobRunState::Held, Some(message.clone()));
    record(now + Duration::seconds(1), JobRunState::Held, Some(message));
    let operator = session(McpCapability::Operator);
    let response = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["detail"]["ahead"], 1);
    assert_eq!(items[0]["detail"]["behind"], 1);
    assert_eq!(items[0]["detail"]["owner_only_commits"][0]["sha"], local);
    assert_eq!(
        items[0]["detail"]["owner_only_commits"][0]["task_id"],
        "ORB-123"
    );
    let id = items[0]["id"].clone();
    let again = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert_eq!(again["items"][0]["id"], id);
    run_git(&repo, &["reset", "--hard", &base]);
    std::fs::write(repo.join("tracked.txt"), "dirty").unwrap();
    let dirty = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert_eq!(dirty["items"][0]["detail"]["ahead"], 0, "{dirty}");
    assert_eq!(dirty["items"][0]["detail"]["tracked_changes"], true);
    run_git(&repo, &["reset", "--hard", &base]);
    let clean = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert!(clean["items"].as_array().unwrap().is_empty(), "{clean}");
    run_git(&repo, &["reset", "--hard", &local]);
    record(now + Duration::seconds(2), JobRunState::Success, None);
    let cleared = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert!(cleared["items"].as_array().unwrap().is_empty(), "{cleared}");
}

#[test]
fn stale_owner_base_attention_continues_after_unavailable_newer_base() {
    let (root, runtime) = attention_runtime();
    let repo = root.path().join("repo");
    let (_, local, remote) = setup_diverged_owner_base(&repo);
    let now = Utc::now() - Duration::minutes(1);
    let record = |at, branch: &str| {
        record_setup_run(
            &runtime,
            SetupRun::at(
                "task_local_pipeline",
                json!({"base_branch": branch, "base_sync": "local"}),
                at,
                JobRunState::Held,
                Some(diverged_hold_message(&repo, branch, &local, &remote)),
            ),
        );
    };
    record(now, "agent-main");
    record(now + Duration::seconds(1), "missing-base");

    let operator = session(McpCapability::Operator);
    let response = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["detail"]["base"], "agent-main", "{response}");
    assert_eq!(items[0]["detail"]["ahead"], 1, "{response}");
    assert_eq!(items[0]["detail"]["behind"], 1, "{response}");

    let degraded = response["degraded"].as_array().unwrap();
    assert_eq!(degraded.len(), 1, "{response}");
    assert_eq!(degraded[0]["kind"], "delivery_stale_base", "{response}");
    assert_eq!(degraded[0]["code"], "source_unavailable", "{response}");
    assert_eq!(degraded[0]["subject"], "missing-base", "{response}");
    let message = degraded[0]["message"].as_str().unwrap();
    assert!(message.contains("missing-base"), "{response}");
    assert!(message.contains("refs/heads/missing-base"), "{response}");
}

#[test]
fn stale_owner_base_attention_ignores_earlier_admitted_overlapping_run_finishing_later() {
    let (root, runtime) = attention_runtime();
    let repo = root.path().join("repo");
    let (_, local, remote) = setup_diverged_owner_base(&repo);
    let now = Utc::now() - Duration::minutes(2);
    let operator = session(McpCapability::Operator);
    let hold_message = diverged_hold_message(&repo, "agent-main", &local, &remote);

    // Run 1 passed worktree_setup at t0 before the hold at t2, and finished
    // implementation/validation later at t3 > t2.
    record_setup_run(
        &runtime,
        SetupRun {
            job_id: "task_local_pipeline",
            input: json!({"base_branch": "agent-main", "base_sync": "local"}),
            run_started_at: now,
            setup_started_at: now,
            setup_finished_at: now + Duration::seconds(1),
            run_finished_at: now + Duration::seconds(10),
            state: JobRunState::Success,
            error: None,
        },
    );
    // Overlapping setup that started before the hold at t2 and finished after it.
    record_setup_run(
        &runtime,
        SetupRun {
            job_id: "task_local_pipeline",
            input: json!({"base_branch": "agent-main", "base_sync": "local"}),
            run_started_at: now,
            setup_started_at: now + Duration::seconds(1),
            setup_finished_at: now + Duration::seconds(6),
            run_finished_at: now + Duration::seconds(12),
            state: JobRunState::Success,
            error: None,
        },
    );
    record_setup_run(
        &runtime,
        SetupRun::at(
            "task_local_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "local"}),
            now + Duration::seconds(5),
            JobRunState::Held,
            Some(hold_message),
        ),
    );

    let response = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    let items = response["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{response}");
    assert_eq!(items[0]["detail"]["base"], "agent-main", "{response}");

    // A genuinely newer local-base admission whose worktree_setup started and
    // succeeded after the hold clears the item.
    record_setup_run(
        &runtime,
        SetupRun {
            job_id: "task_local_pipeline",
            input: json!({"base_branch": "agent-main", "base_sync": "local"}),
            run_started_at: now + Duration::seconds(20),
            setup_started_at: now + Duration::seconds(20),
            setup_finished_at: now + Duration::seconds(21),
            run_finished_at: now + Duration::seconds(25),
            state: JobRunState::Success,
            error: None,
        },
    );
    let cleared = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert!(cleared["items"].as_array().unwrap().is_empty(), "{cleared}");
}

#[test]
fn stale_owner_base_attention_ignores_later_remote_sync_runs() {
    let (root, runtime) = attention_runtime();
    let repo = root.path().join("repo");
    let (_, local, remote) = setup_diverged_owner_base(&repo);
    let now = Utc::now() - Duration::minutes(2);
    let operator = session(McpCapability::Operator);
    let hold_message = diverged_hold_message(&repo, "agent-main", &local, &remote);

    record_setup_run(
        &runtime,
        SetupRun::at(
            "task_local_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "local"}),
            now,
            JobRunState::Held,
            Some(hold_message.clone()),
        ),
    );

    for (offset, (job_id, input)) in [
        (
            "task_pr_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "remote"}),
        ),
        (
            "task_claimed_pr_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "remote"}),
        ),
        (
            "task_local_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "remote"}),
        ),
        ("task_local_pipeline", json!({"base_branch": "agent-main"})),
    ]
    .into_iter()
    .enumerate()
    {
        let at = now + Duration::seconds((offset + 1) as i64);
        record_setup_run(
            &runtime,
            SetupRun::at(job_id, input, at, JobRunState::Success, None),
        );
    }

    let still_held = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    let items = still_held["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{still_held}");
    assert_eq!(items[0]["detail"]["base"], "agent-main", "{still_held}");

    let local_at = now + Duration::seconds(10);
    record_setup_run(
        &runtime,
        SetupRun::at(
            "task_local_pipeline",
            json!({"base_branch": "agent-main", "base_sync": "local"}),
            local_at,
            JobRunState::Success,
            None,
        ),
    );
    let cleared = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert!(cleared["items"].as_array().unwrap().is_empty(), "{cleared}");

    // A claimed local leaf (defaulting base_sync to local) also clears a newer hold.
    let newer_hold_at = now + Duration::seconds(20);
    record_setup_run(
        &runtime,
        SetupRun::at(
            "task_claimed_local_pipeline",
            json!({"base_branch": "agent-main"}),
            newer_hold_at,
            JobRunState::Held,
            Some(hold_message),
        ),
    );
    let reheld = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert_eq!(reheld["items"].as_array().unwrap().len(), 1, "{reheld}");

    let claimed_local_at = now + Duration::seconds(21);
    record_setup_run(
        &runtime,
        SetupRun::at(
            "task_claimed_local_pipeline",
            json!({"base_branch": "agent-main"}),
            claimed_local_at,
            JobRunState::Success,
            None,
        ),
    );
    let cleared_by_claimed = call(&runtime, json!({"kind": "delivery_stale_base"}), &operator);
    assert!(
        cleared_by_claimed["items"].as_array().unwrap().is_empty(),
        "{cleared_by_claimed}"
    );
}
