//! Human crews (`[crews.<name>] kind = "human"`) hold operator work: no drain
//! admits it, whatever its allowlist names; the task pilot never assesses it;
//! only an operator closes it; no experiment arm may draw it; and crew
//! analytics leave it out.

use std::path::{Path, PathBuf};

use orbit_core::OrbitRuntime;
use orbit_core::application::agents::AgentQuery;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::job::TaskPilotSubmitError;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::git_test_support;

const HUMAN_CREW_CONFIG: &str = "[workflow]\ndefault_crew = \"sol\"\n\n\
     [crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n\
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
        self.runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "Fixture work".into(),
                acceptance_criteria: vec!["Done".into()],
                complexity: TaskComplexity::Low,
                context_files: vec!["file:src/lib.rs".into()],
                crew: crew.map(str::to_string),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
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

#[test]
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

#[test]
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

#[test]
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

#[test]
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
