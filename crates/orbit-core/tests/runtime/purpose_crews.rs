//! Purpose crew pools: an explicit crew wins, then the purpose's own pool,
//! then the route the purpose had before purposes existed, and the run's
//! `allowed_crews` restricts every purpose draw. Activities name their purpose
//! through `crew_config_key`; a task names its purpose through its tags.
//! Pool and crew names are fixture policy.

use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskComplexity, TaskPriority, TaskStatus, TaskType,
};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{ActivityCrewDraw, PipelineState};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tempfile::TempDir;

const LOW: &str = "workflow.low_complexity_crews";
const MEDIUM: &str = "workflow.medium_complexity_crews";
const PILOT: &str = "workflow.purpose_crews.task_pilot";
const RECOVERY: &str = "workflow.purpose_crews.failure_recovery";

const CREWS: &str = r#"
[crews.fallback]
provider = "codex"
model = "fixture-fallback"

[crews.low_lane]
provider = "codex"
model = "fixture-low"

[crews.medium_lane]
provider = "codex"
model = "fixture-medium"

[crews.purpose_codex]
provider = "codex"
model = "fixture-purpose-codex"

[crews.purpose_claude]
provider = "claude"
model = "fixture-purpose-claude"

[crews.purpose_gemini]
provider = "gemini"
model = "fixture-purpose-gemini"

[crews.pinned]
provider = "claude"
model = "fixture-pinned"
"#;

/// Complexity pools only: every purpose is unset.
const UNSET: &str = r#"[workflow]
default_crew = "fallback"
low_complexity_crews = ["low_lane"]
medium_complexity_crews = ["medium_lane"]
"#;

/// The same complexity pools, with purpose pools over other crews.
const PURPOSES: &str = r#"[workflow]
default_crew = "fallback"
low_complexity_crews = ["low_lane"]
medium_complexity_crews = ["medium_lane"]

[workflow.purpose_crews]
task_pilot = ["purpose_gemini:500", "purpose_claude:1", "purpose_codex:1"]
failure_recovery = ["purpose_codex:1", "purpose_claude:1000"]
baseline_red_repair = ["purpose_claude"]
"#;

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture(workflow: &str) -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        format!("{workflow}{CREWS}"),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    Fixture {
        _root: root,
        runtime,
        jobs,
    }
}

impl Fixture {
    /// A running pipeline with persisted state, as the engine seeds it.
    fn run(&self, job: &str) -> String {
        let run = self
            .jobs
            .insert_job_run(job, 1, Utc::now(), Some(json!({})), None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn model_for(&self, input: Value) -> String {
        self.runtime
            .agent_crew_config_for_input(&input)
            .unwrap()
            .expect("a crew config")
            .model
            .expect("a model")
    }

    fn draws(&self, run: &str) -> BTreeMap<String, ActivityCrewDraw> {
        self.jobs
            .read_run_state(run)
            .unwrap()
            .unwrap()
            .activity_crew_draws
    }

    fn task(&self, tags: &[&str], crew: Option<&str>) -> orbit_core::Task {
        self.runtime
            .add_task(TaskAddParams {
                title: "Purpose-filed task".into(),
                description: "A task the system files for one purpose.".into(),
                acceptance_criteria: vec!["The crew is observable.".into()],
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                crew: crew.map(str::to_string),
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                system_created: true,
                ..Default::default()
            })
            .unwrap()
    }

    fn mint(&self, name: &str, tags: &[&str], crew: Option<&str>) -> orbit_core::Task {
        self.runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "Purpose fixture".into(),
                schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
                template: AutoTaskTemplate {
                    title: format!("Run {name}"),
                    description: "Exercise purpose crew routing.".into(),
                    acceptance_criteria: vec!["The minted crew is observable.".into()],
                    task_type: TaskType::Chore,
                    tags: tags.iter().map(|tag| tag.to_string()).collect(),
                    required_tools: Vec::new(),
                    context_files: vec!["file:README.md".into()],
                    priority: TaskPriority::Medium,
                    complexity: Some(TaskComplexity::Low),
                    crew: crew.map(str::to_string),
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .unwrap();
        self.runtime.auto_task_mint(name).unwrap()
    }
}

/// The pilot's activity input: `inspection_revision` and `partition_index`
/// are what make it inspect pinned source natively.
fn pilot_input(run: &str) -> Value {
    json!({
        "job_run_id": run, "crew_config_key": PILOT,
        "inspection_revision": "0".repeat(40), "partition_index": 0,
    })
}

fn an_explicit_crew_beats_the_purpose_pool() {
    let fx = fixture(PURPOSES);

    // An activity's own crew is used as it is, and no draw is frozen.
    let run = fx.run("task_pilot_pipeline");
    let mut input = pilot_input(&run);
    input["crew"] = json!("pinned");
    assert_eq!(fx.model_for(input), "fixture-pinned");
    assert!(fx.draws(&run).is_empty(), "{:?}", fx.draws(&run));

    // A task filed for a purpose with its own crew keeps it.
    let task = fx.task(&["baseline-red-repair"], Some("pinned"));
    assert_eq!(task.crew.as_deref(), Some("pinned"));
    assert_eq!(task.crew_source.as_deref(), Some("explicit"));

    // So does an auto-task that names a purpose and a crew.
    let minted = fx.mint(
        "pinned-purpose",
        &["crew-purpose:baseline_red_repair"],
        Some("pinned"),
    );
    assert_eq!(minted.crew.as_deref(), Some("pinned"));
    assert_eq!(minted.crew_source.as_deref(), Some("explicit"));
}

fn a_purpose_pool_beats_the_complexity_pool() {
    let fx = fixture(PURPOSES);

    // The pilot draws from its purpose pool, once per run, keeping only the
    // members that can inspect natively and their weights.
    let run = fx.run("task_pilot_pipeline");
    let model = fx.model_for(pilot_input(&run));
    let draw = fx.draws(&run)[PILOT].clone();
    assert_ne!(draw.crew, "low_lane", "{draw:?}");
    assert_eq!(
        draw.eligible_pool
            .iter()
            .map(|member| (member.name.as_str(), member.weight))
            .collect::<Vec<_>>(),
        [("purpose_claude", 1), ("purpose_codex", 1)],
    );
    assert_eq!(
        draw.source,
        format!("{PILOT}; native inspection, skipped: purpose_gemini")
    );
    for _ in 0..4 {
        assert_eq!(fx.model_for(pilot_input(&run)), model);
    }

    // Failure recovery draws from its own pool, not the medium one.
    let run = fx.run("task_pr_pipeline");
    fx.model_for(json!({"run_id": run, "crew_config_key": RECOVERY}));
    let draws = fx.draws(&run);
    assert_eq!(
        draws.keys().map(String::as_str).collect::<Vec<_>>(),
        [RECOVERY]
    );
    assert_eq!(draws[RECOVERY].source, RECOVERY);

    // A task filed with a purpose's system tag draws from that purpose's pool.
    let task = fx.task(&["baseline-red-repair"], None);
    assert_eq!(task.crew.as_deref(), Some("purpose_claude"));
    assert_eq!(
        task.crew_source.as_deref(),
        Some("purpose:baseline_red_repair")
    );

    // An auto-task names the purpose of the tasks it files with a tag.
    let minted = fx.mint("purpose-chore", &["crew-purpose:baseline_red_repair"], None);
    assert_eq!(minted.crew.as_deref(), Some("purpose_claude"));
    assert_eq!(
        minted.crew_source.as_deref(),
        Some("purpose:baseline_red_repair")
    );

    // A run that froze its draw under the complexity key before the purpose
    // existed keeps that crew when it resumes.
    let run = fx.run("task_pilot_pipeline");
    let mut state = fx.jobs.read_run_state(&run).unwrap().unwrap();
    state.activity_crew_draws.insert(
        LOW.to_string(),
        ActivityCrewDraw {
            crew: "low_lane".into(),
            source: LOW.into(),
            eligible_pool: Vec::new(),
        },
    );
    fx.jobs.write_run_state(&run, &state).unwrap();
    assert_eq!(fx.model_for(pilot_input(&run)), "fixture-low");
    assert!(!fx.draws(&run).contains_key(PILOT));
}

fn an_unset_purpose_resolves_as_it_did_before_purposes() {
    let fx = fixture(UNSET);

    let run = fx.run("task_pilot_pipeline");
    assert_eq!(fx.model_for(pilot_input(&run)), "fixture-low");
    assert_eq!(
        fx.draws(&run)[PILOT].source,
        format!("{LOW}; native inspection")
    );

    let run = fx.run("task_pr_pipeline");
    assert_eq!(
        fx.model_for(json!({"run_id": run, "crew_config_key": RECOVERY})),
        "fixture-medium"
    );
    assert_eq!(fx.draws(&run)[RECOVERY].source, MEDIUM);

    let task = fx.task(&["baseline-red-repair"], None);
    assert_eq!(task.crew.as_deref(), Some("medium_lane"));
    assert_eq!(task.crew_source.as_deref(), Some("pool:medium"));

    // The existing per-purpose keys are the final-recovery and review pools.
    let view = orbit_core::application::config::effective_view(&fx.runtime).unwrap();
    let rows = view["crew_purposes"].as_array().unwrap();
    let row = |purpose: &str| {
        rows.iter()
            .find(|row| row["purpose"] == purpose)
            .unwrap_or_else(|| panic!("no {purpose} row: {rows:?}"))
    };
    assert_eq!(
        row("final_recovery")["key"],
        "workflow.final_recovery_crews"
    );
    assert_eq!(row("review")["key"], "operation.review_crew");
    assert_eq!(row("task_pilot")["configured"], false);
    assert_eq!(row("task_pilot")["resolved_from"], LOW);
    assert_eq!(row("task_pilot")["pool"], json!(["low_lane"]));
    assert_eq!(row("task_pilot")["source"], "workspace");
}

fn the_config_view_names_each_configured_purpose_pool_and_its_source() {
    let fx = fixture(PURPOSES);
    let view = orbit_core::application::config::effective_view(&fx.runtime).unwrap();
    let rows = view["crew_purposes"].as_array().unwrap();
    let recovery = rows
        .iter()
        .find(|row| row["purpose"] == "failure_recovery")
        .unwrap();
    assert_eq!(recovery["configured"], true);
    assert_eq!(recovery["key"], RECOVERY);
    assert_eq!(recovery["resolved_from"], RECOVERY);
    assert_eq!(
        recovery["pool"],
        json!(["purpose_claude:1000", "purpose_codex:1"])
    );
    assert_eq!(recovery["source"], "workspace");
}

fn allowed_crews_restricts_a_purpose_draw() {
    let fx = fixture(PURPOSES);
    // The allowlist leaves only the lighter member of the recovery pool.
    for _ in 0..6 {
        let run = fx.run("task_pr_pipeline");
        assert_eq!(
            fx.model_for(json!({
                "run_id": run, "crew_config_key": RECOVERY,
                "allowed_crews": ["purpose_codex"],
            })),
            "fixture-purpose-codex"
        );
        assert_eq!(
            fx.draws(&run)[RECOVERY]
                .eligible_pool
                .iter()
                .map(|member| member.name.as_str())
                .collect::<Vec<_>>(),
            ["purpose_codex"]
        );
    }

    // An allowlist that excludes every member refuses the draw rather than
    // falling back to another pool, and freezes nothing.
    let run = fx.run("task_pr_pipeline");
    let error = fx
        .runtime
        .agent_crew_config_for_input(&json!({
            "run_id": run, "crew_config_key": RECOVERY,
            "allowed_crews": ["medium_lane"],
        }))
        .expect_err("no permitted member");
    assert!(error.to_string().contains("allowlist"), "{error}");
    assert!(fx.draws(&run).is_empty());

    // An explicit activity crew is still held to the allowlist.
    let error = fx
        .runtime
        .agent_crew_config_for_input(&json!({
            "run_id": run, "crew": "pinned", "crew_config_key": RECOVERY,
            "allowed_crews": ["purpose_codex"],
        }))
        .expect_err("the explicit crew is outside the allowlist");
    assert!(error.to_string().contains("pinned"), "{error}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    purpose_crew_resolution_scenarios,
    an_explicit_crew_beats_the_purpose_pool,
    a_purpose_pool_beats_the_complexity_pool,
    an_unset_purpose_resolves_as_it_did_before_purposes,
    the_config_view_names_each_configured_purpose_pool_and_its_source,
    allowed_crews_restricts_a_purpose_draw
);
