//! Shared runtime/task/run fixture for the agents CLI scenarios:
//! bootstraps a runtime over a disposable fixture store, seeds implemented
//! tasks, records the `JobRun` that worked them, and holds the shared
//! refusal/report assertion helpers the scenarios use.

use std::path::PathBuf;

use chrono::Utc;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{ActorIdentity, JobRun, JobRunState, OrbitRuntime};
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::isolated_cli_fixture::Fixture;

/// Shared runtime/task/run fixture for the agents scenarios: bootstraps a
/// runtime over a disposable fixture store, seeds implemented tasks and
/// records the `JobRun` that worked them.
pub(crate) struct AgentsFixture {
    pub(crate) fixture: Fixture,
    pub(crate) workspace_root: PathBuf,
    pub(crate) runtime: OrbitRuntime,
    pub(crate) workspace: String,
}

/// One seeded task plus the run that worked it.
pub(crate) struct SeedRun<'a> {
    pub(crate) title: String,
    pub(crate) description: &'a str,
    pub(crate) complexity: Option<TaskComplexity>,
    pub(crate) tags: Vec<String>,
    pub(crate) model: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) crew: &'a str,
    pub(crate) state: JobRunState,
}

impl AgentsFixture {
    pub(crate) fn new() -> Self {
        let fixture = Fixture::new();
        let shown = fixture.json(&["workspace", "show", "--format", "json"]);
        let workspace_root = PathBuf::from(shown["orbit_root"].as_str().unwrap());
        // A managed run exports worker authority that would bind the fixture
        // runtime to the live store.
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        let runtime = OrbitRuntime::from_roots(&fixture.root, &workspace_root)
            .unwrap()
            .with_actor(ActorIdentity::human("agents-fixture"));
        let workspace = runtime.workspace_id().unwrap();
        Self {
            fixture,
            workspace_root,
            runtime,
            workspace,
        }
    }

    pub(crate) fn reopen(&self) -> OrbitRuntime {
        let _authority = orbit_common::test_env::unset(
            orbit_common::test_env::INHERITED_AUTHORITY_ENV
                .iter()
                .copied(),
        );
        OrbitRuntime::from_roots(&self.fixture.root, &self.workspace_root).unwrap()
    }

    pub(crate) fn add_task(&self, seed: &SeedRun<'_>) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: seed.title.clone(),
                description: seed.description.into(),
                plan: "implement the change".into(),
                tags: seed.tags.clone(),
                complexity: seed.complexity.unwrap_or(TaskComplexity::Unassessed),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }

    /// An agent's move into in-progress starts the attempt the run worked.
    pub(crate) fn start_task(&self, id: &str, model: &str) {
        self.runtime
            .update_task_with_identity(
                id,
                TaskUpdateParams {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
                Some("claude".into()),
                Some(model.into()),
            )
            .unwrap();
    }

    pub(crate) fn finish_task(&self, id: &str, status: TaskStatus) {
        self.runtime
            .force_update_task_with_identity(
                id,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }

    pub(crate) fn record_run_window(
        &self,
        run_id: &str,
        task_id: &str,
        crew: &str,
        model: &str,
        state: JobRunState,
        window: (chrono::DateTime<Utc>, chrono::DateTime<Utc>),
    ) {
        let (started, finished) = window;
        self.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(
                &self.workspace,
                &JobRun {
                    build: None,
                    run_id: run_id.into(),
                    job_id: "task_pr_pipeline".into(),
                    attempt: 1,
                    state,
                    scheduled_at: started,
                    created_at: started,
                    started_at: Some(started),
                    finished_at: Some(finished),
                    duration_ms: None,
                    pid: None,
                    pid_start_time: None,
                    input: Some(json!({ "task_ids": [task_id] })),
                    retry_source_run_id: None,
                    knowledge_metrics: None,
                    resolved_crew: Some(crew.into()),
                    crew_model: Some(model.into()),
                    steps: Vec::new(),
                    executed_on: None,
                },
                None,
            )
            .unwrap();
    }

    /// Records a run that began an hour before it finished, just now.
    pub(crate) fn record_run(&self, seed: &SeedRun<'_>, task_id: &str) {
        let finished = Utc::now();
        self.record_run_window(
            seed.run_id,
            task_id,
            seed.crew,
            seed.model,
            seed.state,
            (finished - chrono::Duration::hours(1), finished),
        );
    }

    /// Seeds a task moved into in-progress and records the run that worked it.
    pub(crate) fn seed_with_run(&self, seed: &SeedRun<'_>) -> String {
        let id = self.add_task(seed);
        self.start_task(&id, seed.model);
        self.record_run(seed, &id);
        id
    }

    /// Seeds an implemented task with the common fixture defaults and
    /// records the run that worked it.
    pub(crate) fn seed_fixture_run(
        &self,
        title: String,
        complexity: Option<TaskComplexity>,
        crew: &str,
        model: &str,
        run_id: &str,
        state: JobRunState,
    ) -> String {
        self.seed_with_run(&SeedRun {
            title,
            description: "agents fixture",
            complexity,
            tags: Vec::new(),
            model,
            run_id,
            crew,
            state,
        })
    }

    /// [`Self::seed_fixture_run`] for a run that succeeded.
    pub(crate) fn seed_ok(
        &self,
        title: String,
        complexity: Option<TaskComplexity>,
        crew: &str,
        model: &str,
        run_id: &str,
    ) -> String {
        self.seed_fixture_run(title, complexity, crew, model, run_id, JobRunState::Success)
    }

    /// Seeds a task moved into in-progress, with no run recorded yet.
    pub(crate) fn seed_task(&self, title: String, description: &'static str) -> String {
        let seed = SeedRun {
            title,
            description,
            complexity: Some(TaskComplexity::Low),
            tags: Vec::new(),
            model: "model-fixture",
            run_id: "",
            crew: "",
            state: JobRunState::Success,
        };
        let id = self.add_task(&seed);
        self.start_task(&id, seed.model);
        id
    }

    pub(crate) fn begin_execution(
        &self,
        run_id: &str,
        agent: &str,
        crew: &str,
        effort: Option<orbit_types::identity::ReasoningEffort>,
    ) -> i64 {
        self.runtime
            .sqlite_store()
            .unwrap()
            .begin_agent_execution(
                &self.workspace,
                run_id,
                "implement_one",
                agent,
                Some("model-fixture"),
                &orbit_types::telemetry::AgentExecutionIdentity {
                    crew: Some(crew.into()),
                    reasoning_effort: effort,
                },
            )
            .unwrap()
    }

    pub(crate) fn invoke(
        &self,
        run_id: &str,
        activity: &str,
        model: &str,
        task_id: &str,
        execution: Option<i64>,
    ) {
        self.runtime
            .insert_invocation_trace_record(&orbit_core::InvocationInsertParams {
                agent_execution_id: execution,
                job_run_id: run_id.into(),
                account: None,
                activity_id: activity.into(),
                agent: "claude".into(),
                provider: None,
                model: Some(model.into()),
                task_ids: vec![task_id.into()],
                trace: orbit_types::telemetry::InvocationTrace {
                    duration_ms: 60_000,
                    ..Default::default()
                },
            })
            .unwrap();
    }

    pub(crate) fn audit(
        &self,
        run_id: &str,
        event_id: String,
        event_type: &str,
        ts: chrono::DateTime<Utc>,
        payload: Value,
    ) {
        self.runtime
            .sqlite_store()
            .unwrap()
            .insert_v2_audit_event(&orbit_core::V2AuditEventInsertParams {
                workspace_id: self.workspace.clone(),
                event_id,
                source: "fixture".into(),
                schema_version: 1,
                event_type: event_type.into(),
                ts,
                run_id: run_id.into(),
                agent_identity: "system".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: payload.to_string(),
            })
            .unwrap();
    }
}

/// Asserts every `part` is in `haystack`, naming the case on failure.
pub(crate) fn assert_contains(case: &str, haystack: &str, parts: &[&str]) {
    for part in parts {
        assert!(haystack.contains(part), "{case} ({part}): {haystack}");
    }
}

/// Asserts no `part` is in `haystack`, naming the case on failure.
pub(crate) fn assert_excludes(case: &str, haystack: &str, parts: &[&str]) {
    for part in parts {
        assert!(!haystack.contains(part), "{case} ({part}): {haystack}");
    }
}

/// Asserts each `(field, expected)` row of a migrate report, naming the field.
pub(crate) fn assert_report(report: &Value, rows: &[(&str, u64)]) {
    for (field, want) in rows {
        assert_eq!(report[field], json!(want), "report {field}: {report}");
    }
}

/// Runs an `orbit.*` tool through the CLI and returns its full JSON payload.
pub(crate) fn tool_json(fixture: &Fixture, tool: &str, input: &Value) -> Value {
    fixture.json(&["tool", "run", tool, "--full", "--input", &input.to_string()])
}

/// Runs `args` expecting failure and returns its stderr text.
pub(crate) fn refused_stderr(fixture: &Fixture, args: &[&str]) -> String {
    let out = fixture
        .command(args)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs `args` expecting failure and returns stdout and stderr combined.
pub(crate) fn refused_text(fixture: &Fixture, args: &[&str]) -> String {
    let out = fixture
        .command(args)
        .assert()
        .failure()
        .get_output()
        .clone();
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Runs a tool expecting failure and returns its JSON stderr payload.
pub(crate) fn refused_tool(fixture: &Fixture, tool: &str, input: Value) -> Value {
    let out = fixture
        .command(&["tool", "run", tool, "--input", &input.to_string()])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    serde_json::from_slice(&out).unwrap()
}
