//! Human crews (`[crews.<name>] kind = "human"`) hold operator work: no drain
//! admits it, whatever its allowlist names; the task pilot never assesses it;
//! only an operator closes it; no experiment arm may draw it; and crew
//! analytics leave it out.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_core::application::agents::AgentQuery;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::job::TaskPilotSubmitError;
use orbit_core::application::task::{
    TaskAddParams, TaskUpdateAuthority, TaskUpdateParams, TaskWriteScreen,
};
use orbit_core::runtime::engine::crew::BundleCrews;
use orbit_core::{JobRunState, OrbitError, OrbitRuntime};
use orbit_core::{TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::Task;
use orbit_types::workflow::{JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_test_support;

const HUMAN_CREW_CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
     [crews.luna]\nprovider = \"codex\"\nmodel = \"luna-model\"\n\n\
     [crews.dk]\nkind = \"human\"\ndescription = \"Daniel's own work\"\n\n\
     [experiments]\nenabled = true\n";

struct HumanCrewFixture {
    _root: TempDir,
    repo: PathBuf,
    runtime: OrbitRuntime,
}

impl HumanCrewFixture {
    /// A committed Git repository whose workspace config declares the agent
    /// crew `sol` and the human crew `dk`.
    fn new() -> Self {
        crate::scenarios::require_isolated_mutable_fixture(
            "human_crew_work::HumanCrewFixture::new",
        );
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("global");
        let repo = root.path().join("repo");
        let workspace = repo.join(".orbit");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("config.toml"), HUMAN_CREW_CONFIG).unwrap();
        std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
        std::fs::write(repo.join("src/lib.rs"), "pub fn renew() {}\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "fixture@example.com"],
            &["config", "user.name", "Fixture"],
            &["config", "commit.gpgsign", "false"],
            &["add", "-A"],
            &["commit", "-q", "-m", "fixture"],
        ] {
            human_crew_git(&repo, args);
        }
        let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
        Self {
            _root: root,
            repo,
            runtime,
        }
    }

    fn task(&self, title: &str, crew: Option<&str>) -> String {
        self.task_in(title, crew, TaskStatus::Backlog)
    }

    fn task_in(&self, title: &str, crew: Option<&str>, status: TaskStatus) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Fixture work".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                context_files: vec!["file:src/lib.rs".into()],
                crew: crew.map(str::to_string),
                status: Some(status),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    }

    /// The operator surfaces' status write (`orbit task update --status`, the
    /// dashboard's PATCH): the lifecycle authority, with no `--force`.
    fn operator_status(
        &self,
        task: &str,
        status: TaskStatus,
    ) -> Result<Task, orbit_core::OrbitError> {
        self.runtime
            .update_task_screened(
                task,
                TaskUpdateParams {
                    status: Some(status),
                    ..Default::default()
                },
                &TaskWriteScreen::unbound(false),
                TaskUpdateAuthority::Lifecycle,
                None,
                None,
            )
            .map(|written| written.task)
    }

    fn update(&self, input: Value) -> Result<Value, orbit_core::OrbitError> {
        self.runtime.run_tool("orbit.task.update", input)
    }

    fn prepare_pilot(&self, task_ids: Option<&[&str]>) -> Value {
        let mut input = json!({
            "workspace_path": self.repo.canonicalize().unwrap(),
            "base_branch": "main",
        });
        if let Some(task_ids) = task_ids {
            input["task_ids"] = json!(task_ids);
        }
        self.runtime
            .run_deterministic(
                "prepare_task_pilot",
                &json!({}),
                &input,
                ToolContext::default(),
            )
            .expect("prepare task pilot")
    }
}

fn human_crew_git(repo: &Path, args: &[&str]) {
    let output = git_test_support::run(repo, args).unwrap();
    assert!(output.success, "git {args:?} failed: {}", output.stderr);
}

fn excluded_reason(prepared: &Value, task: &str) -> Option<String> {
    prepared["excluded"]
        .as_array()?
        .iter()
        .find(|entry| entry["task_id"] == task)
        .and_then(|entry| entry["reason"].as_str())
        .map(str::to_string)
}

fn an_agent_cannot_close_a_human_crew_task_and_an_operator_can() {
    let fx = HumanCrewFixture::new();
    let task = fx.task("Renew the domain", Some("dk"));
    fx.update(json!({ "id": task, "status": "in-progress" }))
        .expect("operator starts the work");
    fx.update(json!({ "id": task, "status": "review", "execution_summary": "Renewed." }))
        .expect("operator hands it to review");

    let refusal = fx
        .update(json!({
            "id": task,
            "status": "done",
            "model": "claude",
            "execution_summary": "Renewed.",
        }))
        .expect_err("an agent must not close operator work");
    assert!(
        refusal
            .to_string()
            .contains("only an operator may move it to done"),
        "{refusal}"
    );
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Review
    );

    fx.update(json!({ "id": task, "status": "done" }))
        .expect("the operator closes it");
    assert_eq!(fx.runtime.get_task(&task).unwrap().status, TaskStatus::Done);
}

/// A human crew's task is operator work done outside any pipeline, so the
/// operator closes it from any status without a plan, review, evidence or
/// `--force`; an agent or MCP caller keeps the lifecycle on the same task, so
/// assigning a human crew never skips review. Each bypass is a forced-style
/// history entry naming the actor, the move and the human crew.
fn an_operator_moves_a_human_crew_task_to_any_status_and_an_agent_cannot() {
    let fx = HumanCrewFixture::new();

    // An agent crew's task keeps the table for the operator too.
    let agent_task = fx.task_in("Rename renew", Some("sol"), TaskStatus::Proposed);
    let refused = fx
        .operator_status(&agent_task, TaskStatus::Done)
        .expect_err("done is reachable only from review on an agent crew's task");
    assert!(
        refused
            .to_string()
            .contains("'done' is reachable only from 'review'"),
        "{refused}"
    );

    let task = fx.task_in("Renew the domain", Some("dk"), TaskStatus::Proposed);
    for (attempt, expected) in [
        (
            fx.update(json!({ "id": task, "status": "done", "model": "claude", "execution_summary": "Renewed." })),
            "cannot move from 'proposed' to 'done'",
        ),
        (
            fx.update(json!({ "id": task, "status": "done" })),
            "'done' is reachable only from 'review'",
        ),
        (
            fx.update(json!({ "id": task, "status": "in-progress" })),
            "requires a non-empty execution plan",
        ),
    ] {
        let refusal = attempt.expect_err("the tool surface keeps the lifecycle");
        assert!(refusal.to_string().contains(expected), "{refusal}");
        assert_eq!(
            fx.runtime.get_task(&task).unwrap().status,
            TaskStatus::Proposed
        );
    }

    let done = fx
        .operator_status(&task, TaskStatus::Done)
        .expect("the operator closes a proposed human-crew task directly");
    assert_eq!(done.status, TaskStatus::Done);
    let history = fx.runtime.get_task_history(&task).unwrap();
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.event == "forced")
        .expect("the bypass is recorded like a forced transition");
    assert_eq!(entry.from_status, Some(TaskStatus::Proposed));
    assert_eq!(entry.to_status, Some(TaskStatus::Done));
    assert!(!entry.by.is_empty());
    assert!(
        entry
            .note
            .as_deref()
            .is_some_and(|note| note.contains("human-crew bypass") && note.contains("'dk'")),
        "{entry:?}"
    );

    // Out of `done` the existing force rules still apply.
    let reopen = fx
        .operator_status(&task, TaskStatus::Backlog)
        .expect_err("reopening needs --force");
    assert!(reopen.to_string().contains("cannot move from"), "{reopen}");
}

fn the_task_pilot_never_assesses_a_human_crew_task() {
    let fx = HumanCrewFixture::new();
    let human = fx.task("Renew the domain", Some("dk"));
    let agent = fx.task("Rename renew", None);

    let automatic = fx.prepare_pilot(None);
    assert_eq!(
        excluded_reason(&automatic, &human).as_deref(),
        Some("human_crew"),
        "{automatic:#}"
    );
    assert_ne!(
        excluded_reason(&automatic, &agent).as_deref(),
        Some("human_crew"),
        "only the human crew's task is excluded as one: {automatic:#}"
    );

    let explicit = fx.prepare_pilot(Some(&[human.as_str()]));
    assert_eq!(
        excluded_reason(&explicit, &human).as_deref(),
        Some("human_crew"),
        "{explicit:#}"
    );
    assert_eq!(explicit["task_ids"], json!([]), "{explicit:#}");

    let refusal = fx
        .runtime
        .submit_explicit_task_pilot(&human, None, JobRunTrigger::cli())
        .expect_err("an explicit pilot request is refused");
    let TaskPilotSubmitError::Refused(refusal) = refusal else {
        panic!("expected a preflight refusal, got {refusal}");
    };
    assert_eq!(refusal.code(), "task_pilot_human_crew");
    // Nothing was recorded on the task: no assessment, warning or hold.
    assert!(fx.runtime.get_task_comments(&human).unwrap().is_empty());
}

/// [ORB-23955] A task-pilot run whose task crew is human does not fail at
/// start, does not record that crew, and does not pilot the task.
fn a_task_pilot_run_keeps_an_agent_crew_when_its_task_crew_is_human() {
    let fx = HumanCrewFixture::new();
    let human = fx.task_in("Renew the domain", Some("dk"), TaskStatus::Proposed);
    let agent = fx.task_in("Rename renew", Some("luna"), TaskStatus::Proposed);
    // Select before any task-pilot run is inserted. A pending pilot run would
    // hold the agent task as `pilot_running` and hide this drain's own choice.
    let drain = fx
        .runtime
        .stores()
        .jobs()
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({ "approve_proposed": true })),
            None,
        )
        .unwrap();
    fx.runtime
        .write_run_state(
            &drain.run_id,
            &PipelineState::new(
                drain.run_id.clone(),
                "workspace_auto_pipeline".into(),
                json!({}),
            ),
        )
        .unwrap();
    let selection = fx
        .runtime
        .run_deterministic(
            "select_proposed_approvals",
            &json!({}),
            &json!({ "approve_proposed": true, "run_id": drain.run_id }),
            ToolContext::default(),
        )
        .expect("select proposed approvals");
    let chosen = selection["task_ids"].as_array().unwrap();
    assert!(
        !chosen.iter().any(|id| id == &human),
        "the drain does not pilot the human-crew task: {selection:#}"
    );
    assert!(
        chosen.iter().any(|id| id == &agent),
        "the drain still pilots the agent task: {selection:#}"
    );
    let held = selection["held"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == human)
        .expect("the human task is reported as excluded");
    assert_eq!(held["reason"], "human_crew", "{held}");
    assert!(
        fx.runtime
            .get_task_history(&human)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "task_pilot_unresolved"),
        "the drain records no pilot hold on the human-crew task"
    );

    let pilot_yaml =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/jobs/task_pilot_pipeline.yaml");
    // Outside the fixture repository: prepare reads that worktree, and an
    // extra job file there is not part of the crew-resolution case.
    let other_yaml = fx.repo.parent().unwrap().join("other-job.yaml");
    std::fs::write(
        &other_yaml,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  kind: workflow\n  steps:\n    - id: work\n      target: activity:task_local\n",
    )
    .unwrap();

    let record = |input: &Value, yaml: &Path, job: &str| {
        let run = fx
            .runtime
            .stores()
            .jobs()
            .insert_job_run(job, 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        let resolved =
            fx.runtime
                .record_run_crew_for_job(&run.run_id, input, yaml, BundleCrews::Unanimous);
        (run.run_id, resolved)
    };

    let human_input = json!({ "task_ids": [human] });
    let (human_run, resolved) = record(&human_input, &pilot_yaml, "task_pilot_pipeline");
    resolved.expect("a human task crew does not deny the pilot run");
    let stored = fx
        .runtime
        .stores()
        .jobs()
        .get_job_run(&human_run)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.state,
        JobRunState::Pending,
        "the pilot run is not failed"
    );
    assert!(stored.finished_at.is_none(), "{stored:?}");
    assert_eq!(
        stored.resolved_crew.as_deref(),
        Some("sol"),
        "the run keeps the default agent crew"
    );

    let (override_run, denied) = record(
        &json!({ "task_ids": [human], "crew": "dk" }),
        &pilot_yaml,
        "task_pilot_pipeline",
    );
    let denied = denied.expect_err("an explicit human crew is still refused");
    assert!(
        matches!(denied, OrbitError::PolicyDenied(ref message) if message.contains("human crew")),
        "{denied}"
    );
    let override_stored = fx
        .runtime
        .stores()
        .jobs()
        .get_job_run(&override_run)
        .unwrap()
        .unwrap();
    assert_eq!(override_stored.state, JobRunState::Pending);
    assert!(override_stored.resolved_crew.is_none());

    let (_, other) = record(&human_input, &other_yaml, "task_local_pipeline");
    let other = other.expect_err("a non-pilot job still refuses a human task crew");
    assert!(
        matches!(other, OrbitError::PolicyDenied(ref message) if message.contains("task.crew")),
        "{other}"
    );

    let (agent_run, agent_resolved) = record(
        &json!({ "task_ids": [agent] }),
        &pilot_yaml,
        "task_pilot_pipeline",
    );
    agent_resolved.expect("an agent task crew still resolves");
    assert_eq!(
        fx.runtime
            .stores()
            .jobs()
            .get_job_run(&agent_run)
            .unwrap()
            .unwrap()
            .resolved_crew
            .as_deref(),
        Some("luna"),
        "the pilot keeps an agent task crew"
    );

    let prepared = fx.prepare_pilot(Some(&[human.as_str(), agent.as_str()]));
    assert_eq!(
        excluded_reason(&prepared, &human).as_deref(),
        Some("human_crew"),
        "{prepared:#}"
    );
    let selected = prepared["task_ids"].as_array().unwrap();
    assert!(
        !selected.iter().any(|id| id == &human),
        "the human-crew task is not piloted: {prepared:#}"
    );
    assert!(
        selected.iter().any(|id| id == &agent),
        "the agent task is still selected: {prepared:#}"
    );
    assert!(
        fx.runtime.get_task_comments(&human).unwrap().is_empty(),
        "pilot records nothing on the human-crew task"
    );

    let mixed = json!({ "task_ids": [human, agent] });
    let mixed_run = fx
        .runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_pilot_pipeline",
            1,
            Utc::now(),
            Some(mixed.clone()),
            None,
        )
        .unwrap();
    fx.runtime
        .record_run_crew_for_job(
            &mixed_run.run_id,
            &mixed,
            &pilot_yaml,
            BundleCrews::DrainPartitions,
        )
        .expect("a partition whose first crew is human keeps the agent crew");
    assert_eq!(
        fx.runtime
            .stores()
            .jobs()
            .get_job_run(&mixed_run.run_id)
            .unwrap()
            .unwrap()
            .resolved_crew
            .as_deref(),
        Some("luna")
    );
}

fn an_experiment_arm_cannot_draw_a_human_crew() {
    let fx = HumanCrewFixture::new();
    let actor = ExperimentActor {
        actor: "human-crew-test".into(),
        reason: None,
    };
    let overlay = json!({ "crew": { "pool": [{ "crew": "sol", "weight": 1 }, { "crew": "dk", "weight": 1 }] } });
    let experiment = fx
        .runtime
        .create_experiment(
            "human-arm",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment", "overlay": overlay }
                ])),
                primary_metric: Some("total_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    let refusal = fx
        .runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .expect_err("a human crew in an arm pool refuses the start");
    let message = refusal.to_string();
    assert!(message.contains("which is a human crew"), "{message}");
    assert!(message.contains("arm `treatment`"), "{message}");
}

fn crew_analytics_leave_human_crew_tasks_out() {
    let fx = HumanCrewFixture::new();
    let human = fx.task("Renew the domain", Some("dk"));
    let agent = fx.task("Rename renew", Some("sol"));
    for task in [&human, &agent] {
        fx.update(json!({ "id": task, "status": "in-progress" }))
            .unwrap();
        fx.update(json!({ "id": task, "status": "review", "execution_summary": "Done." }))
            .unwrap();
        fx.update(json!({ "id": task, "status": "done" })).unwrap();
    }
    let export = fx
        .runtime
        .agent_per_task_export(
            &AgentQuery::parse(Some("all"), None)
                .and_then(|query| query.with_execution(Some("all")))
                .unwrap(),
        )
        .unwrap();
    let scored = |task: &str| {
        export["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["task_id"] == task)
    };
    assert!(
        scored(&agent),
        "the agent crew's task is scored: {export:#}"
    );
    assert!(
        !scored(&human),
        "a human crew's task scores no agent: {export:#}"
    );
}

fn filing_a_human_crew_task_warns_unless_the_body_is_short_with_steps_commands_and_verify() {
    let fx = HumanCrewFixture::new();
    let file = |crew: &str, description: &str| -> Vec<String> {
        let added = fx
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "workspace": "test",
                    "title": "Rotate the expired token",
                    "description": description,
                    "complexity": "low",
                    "crew": crew,
                }),
            )
            .expect("task add");
        added["warnings"]
            .as_array()
            .map(|warnings| {
                warnings
                    .iter()
                    .filter_map(|warning| warning.as_str())
                    .filter(|warning| warning.starts_with("human-crew task"))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };

    let compliant = "Rotate the expired GitHub token; CI sweeps fail without it.\n\n\
        1. Create a token: https://github.com/settings/tokens\n\
        2. Install it:\n\n```sh\ngh auth login --with-token < token.txt\n```\n\n\
        Verify:\n\n```sh\ngh auth status\n```";
    assert!(file("dk", compliant).is_empty(), "compliant body warns");

    let wall = format!(
        "{}\n",
        "Investigate the failing credential rotation at length. ".repeat(60)
    );
    let warnings = file("dk", &wall);
    for expected in [
        "numbered steps",
        "fenced shell block",
        "verify command",
        "limit",
    ] {
        assert!(
            warnings.iter().any(|warning| warning.contains(expected)),
            "missing `{expected}` warning: {warnings:?}"
        );
    }

    assert!(
        file("sol", &wall).is_empty(),
        "an agent crew's task is not held to the human format"
    );
}

crate::scenario_table!(
    human_crew_work,
    an_agent_cannot_close_a_human_crew_task_and_an_operator_can,
    an_operator_moves_a_human_crew_task_to_any_status_and_an_agent_cannot,
    the_task_pilot_never_assesses_a_human_crew_task,
    a_task_pilot_run_keeps_an_agent_crew_when_its_task_crew_is_human,
    an_experiment_arm_cannot_draw_a_human_crew,
    crew_analytics_leave_human_crew_tasks_out,
    filing_a_human_crew_task_warns_unless_the_body_is_short_with_steps_commands_and_verify,
);
