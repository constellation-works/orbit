//! System work without a lane of its own [ORB-15663]: auto-task templates
//! without a crew mint tasks that draw from the complexity pool for their
//! complexity, and the task pilot and recovery activities draw their crew
//! from the pool their `crew_config_key` names, once per run, recorded in the
//! run's state. The retired `system` crew name routes through the pools too.
//! Pool and crew names are fixture policy.

use chrono::{Duration, Utc};
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitRuntime,
    TaskComplexity, TaskPriority, TaskStatus, TaskType,
};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{ActivityCrewDraw, PipelineState};
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;

use super::dispatch_admission::isolated;

const LOW: &str = "workflow.low_complexity_crews";
const MEDIUM: &str = "workflow.medium_complexity_crews";

/// `medium` is left empty so a draw from it falls back to the default crew.
const CONFIG: &str = r#"[workflow]
default_crew = "fallback"
low_complexity_crews = ["low_codex:3", "low_claude:1", "low_parked:1"]
medium_complexity_crews = []
hard_complexity_crews = ["hard_lane"]

[crews.fallback]
provider = "codex"
model = "fixture-fallback"

[crews.low_codex]
provider = "codex"
model = "fixture-low-codex"

[crews.low_claude]
provider = "claude"
model = "fixture-low-claude"

[crews.low_parked]
provider = "codex"
model = "fixture-low-parked"
enabled = false

[crews.hard_lane]
provider = "claude"
model = "fixture-hard"
"#;

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(repo.join(".orbit/config.toml"), CONFIG).unwrap();
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

    /// The crew an activity input resolves to, by model.
    fn model_for(&self, input: Value) -> String {
        self.runtime
            .agent_crew_config_for_input(&input)
            .unwrap()
            .expect("a crew config")
            .model
            .expect("a model")
    }

    fn draws(&self, run: &str) -> std::collections::BTreeMap<String, ActivityCrewDraw> {
        self.jobs
            .read_run_state(run)
            .unwrap()
            .unwrap()
            .activity_crew_draws
    }

    fn mint(&self, name: &str, complexity: TaskComplexity, crew: Option<&str>) -> orbit_core::Task {
        self.runtime
            .auto_task_add(AutoTaskAddParams {
                name: name.into(),
                description: "System work fixture".into(),
                schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
                template: AutoTaskTemplate {
                    title: format!("Run {name}"),
                    description: "Exercise template crew routing.".into(),
                    acceptance_criteria: vec!["The minted crew is observable.".into()],
                    task_type: TaskType::Chore,
                    tags: Vec::new(),
                    required_tools: Vec::new(),
                    context_files: vec!["file:README.md".into()],
                    priority: TaskPriority::Medium,
                    complexity: Some(complexity),
                    crew: crew.map(str::to_string),
                    status: TaskStatus::Backlog,
                },
                dedupe: DedupePolicy::SkipIfOpen,
            })
            .unwrap();
        self.runtime.auto_task_mint(name).unwrap()
    }
}

#[test]
fn auto_task_templates_without_a_crew_draw_the_pool_for_their_complexity() {
    if !isolated(
        "system_work_crews::auto_task_templates_without_a_crew_draw_the_pool_for_their_complexity",
    ) {
        return;
    }
    let fx = fixture();

    let hard = fx.mint("hard-review", TaskComplexity::Hard, None);
    assert_eq!(hard.crew.as_deref(), Some("hard_lane"));
    assert_eq!(hard.crew_source.as_deref(), Some("pool:hard"));

    // A template still naming the retired crew routes the same way.
    let retired = fx.mint("retired-lane", TaskComplexity::Hard, Some("system"));
    assert_eq!(retired.crew.as_deref(), Some("hard_lane"));
    assert_eq!(retired.crew_source.as_deref(), Some("pool:hard"));

    // An explicit template crew still wins over the pool.
    let pinned = fx.mint("pinned-review", TaskComplexity::Hard, Some("low_codex"));
    assert_eq!(pinned.crew.as_deref(), Some("low_codex"));
    assert_eq!(pinned.crew_source.as_deref(), Some("explicit"));
}

#[test]
fn an_activity_pool_draw_is_frozen_per_run_and_skips_disabled_and_limited_crews() {
    if !isolated(
        "system_work_crews::an_activity_pool_draw_is_frozen_per_run_and_skips_disabled_and_limited_crews",
    ) {
        return;
    }
    let fx = fixture();
    let run = fx.run("task_pilot_pipeline");
    let first = fx.model_for(json!({"job_run_id": run, "crew_config_key": LOW}));
    let draw = fx.draws(&run)[LOW].clone();
    assert_eq!(draw.source, LOW);
    assert_eq!(
        draw.eligible_pool
            .iter()
            .map(|member| (member.name.as_str(), member.weight))
            .collect::<Vec<_>>(),
        [("low_claude", 1), ("low_codex", 3)],
        "weights are kept and the disabled member is skipped"
    );
    assert_eq!(first, fx.model_for(json!({"crew": draw.crew})));
    // Every later dispatch of the key in this run reuses the frozen crew.
    for _ in 0..8 {
        assert_eq!(
            fx.model_for(json!({"run_id": run, "crew_config_key": LOW})),
            first
        );
    }

    // A crew at a provider limit is skipped while another member is left,
    // and the recorded draw says so.
    fx.runtime
        .record_provider_limit(&ProviderLimitObservation {
            provider: "claude".into(),
            model: None,
            window: Some("five_hour".into()),
            exhausted: true,
            source: ProviderLimitSource::Event,
            resets_at: Some(Utc::now() + Duration::hours(2)),
            used_percent: Some(100.0),
            window_minutes: Some(300),
            gating: true,
            partial: false,
            observed_at: Utc::now(),
            run_id: None,
            crew: None,
            detail: String::new(),
        })
        .unwrap();
    let limited = fx.run("task_pilot_pipeline");
    assert_eq!(
        fx.model_for(json!({"job_run_id": limited, "crew_config_key": LOW})),
        "fixture-low-codex"
    );
    let draw = fx.draws(&limited)[LOW].clone();
    assert_eq!(draw.crew, "low_codex");
    assert!(
        draw.source.starts_with(LOW) && draw.source.contains("provider limit"),
        "{}",
        draw.source
    );
}

#[test]
fn an_empty_pool_falls_back_to_the_default_crew_and_the_run_says_so() {
    if !isolated(
        "system_work_crews::an_empty_pool_falls_back_to_the_default_crew_and_the_run_says_so",
    ) {
        return;
    }
    let fx = fixture();
    let run = fx.run("task_pr_pipeline");
    assert_eq!(
        fx.model_for(json!({"run_id": run, "crew_config_key": MEDIUM})),
        "fixture-fallback"
    );
    let draw = fx.draws(&run)[MEDIUM].clone();
    assert_eq!(draw.crew, "fallback");
    assert!(
        draw.source.contains("workflow.default_crew") && draw.source.contains(MEDIUM),
        "the recorded draw names the fallback and the empty pool: {}",
        draw.source
    );
}

#[test]
fn the_retired_system_route_draws_from_the_medium_pool() {
    if !isolated("system_work_crews::the_retired_system_route_draws_from_the_medium_pool") {
        return;
    }
    let fx = fixture();
    for input in [
        json!({"crew_config_key": "workflow.system_crew"}),
        json!({"crew": "system"}),
    ] {
        let run = fx.run("task_pr_pipeline");
        let mut input = input;
        input["run_id"] = json!(run);
        assert_eq!(fx.model_for(input.clone()), "fixture-fallback", "{input}");
        let draws = fx.draws(&run);
        assert_eq!(
            draws.keys().map(String::as_str).collect::<Vec<_>>(),
            [MEDIUM],
            "{input}"
        );
    }
}
