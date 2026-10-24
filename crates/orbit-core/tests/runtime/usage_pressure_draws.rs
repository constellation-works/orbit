//! [ORB-17770] Under `workflow.crew_selection = "usage_pressure"`, task
//! creation, complexity re-rate and admission draws weigh each pool member by
//! its crew's weekly usage pressure, and admission draws a system-chosen crew
//! again from its whole pool. Under `static`, or with the keys unset, every
//! draw and its recorded selection is what it was before.
//!
//! Weekly readings are seeded into the host's provider-limit store the way a
//! run's telemetry records them. A selection is read from a replay's admitted
//! run input, the same admission every delivery submission runs.
//!
//! Draws are random. Where a test needs one outcome, its config drives the
//! losing member to one ticket against about a billion, so the outcome is
//! fixed in practice without a seed hook.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus, WorkspaceRuntimeBinding,
};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{JobRunState, JobRunTrigger};
use serde_json::{Value, json};
use tempfile::TempDir;

/// A provider reading at the public telemetry boundary. Used by the other
/// pool-draw regression scenarios as well as task admission.
pub(crate) fn record_week(
    runtime: &OrbitRuntime,
    provider: &str,
    used_percent: f64,
    resets_in: Duration,
) {
    runtime
        .record_provider_limit(&ProviderLimitObservation {
            provider: provider.into(),
            account: None,
            model: None,
            window: Some("seven_day".into()),
            exhausted: false,
            source: ProviderLimitSource::Event,
            resets_at: Some(Utc::now() + resets_in),
            used_percent: Some(used_percent),
            window_minutes: Some(10080),
            gating: true,
            partial: false,
            observed_at: Utc::now(),
            run_id: None,
            crew: None,
            detail: String::new(),
        })
        .unwrap();
}

/// Configured weights stay observable beside the actual pressure-adjusted
/// weights; membership and parked-member filtering remain the caller's.
pub(crate) fn assert_pool(pool: &Value, expected: &[(&str, u32, f64)]) {
    let members = pool
        .as_array()
        .unwrap_or_else(|| panic!("missing eligible pool: {pool}"));
    assert_eq!(members.len(), expected.len(), "{pool}");
    for (name, weight, effective) in expected {
        let member = members
            .iter()
            .find(|member| member["name"] == *name)
            .unwrap();
        assert_eq!(member["weight"], *weight, "{pool}");
        assert_eq!(member["effective_weight"], *effective, "{pool}");
        assert!(
            member["pressure"]
                .as_f64()
                .is_some_and(|pressure| pressure > 0.0),
            "{pool}"
        );
    }
}

/// Two Claude crews and a Codex crew, `pool` as the hard-complexity pool,
/// `purpose` as the baseline-red repair pool, and `extra` under `[workflow]`.
fn config(pool: &str, purpose: &str, extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\nhard_complexity_crews = {pool}\n{extra}\n\n\
         [workflow.purpose_crews]\nbaseline_red_repair = {purpose}\n\n\
         [crews.opus]\nprovider = \"claude\"\nmodel = \"opus-model\"\n\n\
         [crews.sonnet]\nprovider = \"claude\"\nmodel = \"sonnet-model\"\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
         [review]\nbefore_pr = false\n"
    )
}

/// Pressure decides outright: a Claude crew at ×10⁶ against a Codex crew at
/// the 10⁻⁶ floor, so the Codex member holds one ticket in about a billion.
const DECISIVE: &str = "crew_selection = \"usage_pressure\"\n\
                        usage_pressure_cap = 1000000\n\
                        usage_pressure_strength = 10";

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        std::fs::create_dir_all(root.path().join("global/resources/jobs")).unwrap();
        std::fs::create_dir_all(root.path().join("repo/.orbit")).unwrap();
        let fixture = Self { root };
        fixture.configure(config);
        fixture
    }

    fn configure(&self, config: &str) {
        std::fs::write(self.root.path().join("repo/.orbit/config.toml"), config).unwrap();
    }

    fn runtime(&self) -> OrbitRuntime {
        let repo = self.root.path().join("repo");
        OrbitRuntime::from_roots_with_binding(
            &self.root.path().join("global"),
            &repo.join(".orbit"),
            WorkspaceRuntimeBinding {
                logical_workspace_id: "ws_usage_pressure".into(),
                task_partition_id: "ws_usage_pressure".into(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: repo,
                ship_mode: ShipMode::Local,
                base_branch: Some("main".into()),
            },
        )
        .unwrap()
        .with_host_resource_probe(super::dispatch_admission::PressureProbe::calm())
    }

    /// A backlog task of `complexity`, tagged `tags`, on `crew` when one is
    /// named. Its own context file keeps tasks from locking each other.
    fn task(
        &self,
        name: &str,
        complexity: TaskComplexity,
        crew: Option<&str>,
        tags: &[&str],
    ) -> Task {
        std::fs::write(self.root.path().join("repo").join(name), "fixture\n").unwrap();
        self.runtime()
            .add_task(TaskAddParams {
                title: format!("Usage pressure fixture {name}"),
                description: "A task whose crew draw weighs usage pressure.".into(),
                acceptance_criteria: vec!["Delivered.".into()],
                plan: "1. Deliver it.".into(),
                complexity,
                context_files: vec![format!("file:{name}")],
                task_type: Some(orbit_core::TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                tags: tags.iter().map(|tag| (*tag).to_string()).collect(),
                crew: crew.map(ToOwned::to_owned),
                ..Default::default()
            })
            .unwrap()
    }

    /// Record a weekly reading of `provider`: `used_percent` used, resetting
    /// in `resets_in`.
    fn week(&self, provider: &str, used_percent: f64, resets_in: Duration) {
        record_week(&self.runtime(), provider, used_percent, resets_in);
    }

    /// The run input a replay of a failed delivery, submitted with `input`,
    /// is admitted with.
    fn admit(&self, input: Value) -> Value {
        std::fs::write(
            self.root
                .path()
                .join("global/resources/jobs/task_auto_pipeline.yaml"),
            "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_auto_pipeline\nspec:\n  \
             state: enabled\n  steps: []\n",
        )
        .unwrap();
        let runtime = self.runtime();
        let jobs = orbit_store::compose::workspace_job_run_store(
            runtime.sqlite_store().unwrap(),
            runtime.workspace_id().unwrap(),
        );
        let source = jobs
            .insert_job_run("task_auto_pipeline", 1, Utc::now(), Some(input), None)
            .unwrap();
        jobs.mark_job_run_running(&source.run_id, Utc::now(), std::process::id())
            .unwrap();
        jobs.finalize_job_run(&source.run_id, JobRunState::Failed, Utc::now(), Some(1))
            .unwrap();
        let replay = runtime
            .submit_replay_run(&source.run_id, None, None, JobRunTrigger::dashboard())
            .unwrap();
        jobs.get_job_run(&replay.run_id)
            .unwrap()
            .unwrap()
            .input
            .unwrap()
    }

    /// The task's `crew_redrawn` history notes, oldest first.
    fn redraws(&self, task: &str) -> Vec<String> {
        self.runtime()
            .get_task_history(task)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.event == "crew_redrawn")
            .map(|entry| entry.note.unwrap_or_default())
            .collect()
    }
}

/// Workers are harmless substitutes: admission is complete before a run is
/// persisted.
fn substitute_worker() {
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".into(),
    ]);
}

const POOL: &str = r#"["opus:50", "sonnet:25", "sol:25"]"#;

/// Claude 10% used with six hours left (pressure about 25) and Codex 0% used
/// with the week ahead (pressure 1): the task text's worked example.
fn read_example_week(fx: &Fixture) {
    fx.week("claude", 10.0, Duration::hours(6));
    fx.week("codex", 0.0, Duration::days(7));
}

/// Criterion 1: unset, `static`, and `usage_pressure` at strength 0 record
/// the selection exactly as before, whatever the readings, and never
/// re-draw or rewrite a task's assigned crew.
fn static_selection_draws_and_records_as_before() {
    substitute_worker();
    let fx = Fixture::new(&config(r#"["sol"]"#, "[]", ""));
    let assigned = fx.task("assigned.txt", TaskComplexity::Hard, None, &[]);
    assert_eq!(assigned.crew.as_deref(), Some("sol"));
    assert_eq!(assigned.crew_source.as_deref(), Some("pool:hard"));
    // Created before the pool exists, this task draws the whole pool.
    fx.configure(&config("[]", "[]", ""));
    let unpooled = fx.task("unpooled.txt", TaskComplexity::Hard, None, &[]);
    assert_eq!(unpooled.crew_source.as_deref(), Some("default"));
    read_example_week(&fx);

    for extra in [
        "",
        "crew_selection = \"static\"",
        "crew_selection = \"usage_pressure\"\nusage_pressure_strength = 0",
    ] {
        fx.configure(&config(POOL, "[]", extra));
        for _ in 0..5 {
            let admitted = fx.admit(json!({ "task_ids": [assigned.id] }));
            assert_eq!(
                admitted["crew_selection"],
                json!({
                    "task_id": assigned.id, "crew": "sol", "source": "pool:hard",
                    "complexity": "hard", "eligible_pool": [{"name": "sol", "weight": 1}],
                }),
                "[{extra}] the assigned pool member is kept: {admitted}"
            );
            let admitted = fx.admit(json!({ "task_ids": [unpooled.id] }));
            let selection = &admitted["crew_selection"];
            assert_eq!(
                selection["source"], "workflow.hard_complexity_crews",
                "[{extra}] {admitted}"
            );
            assert_eq!(
                selection["eligible_pool"],
                json!([
                    {"name": "opus", "weight": 50},
                    {"name": "sol", "weight": 25},
                    {"name": "sonnet", "weight": 25},
                ]),
                "[{extra}] configured weights, no pressure fields: {admitted}"
            );
        }
        for task in [&assigned, &unpooled] {
            let current = fx.runtime().get_task(&task.id).unwrap();
            assert_eq!(
                current.crew, task.crew,
                "[{extra}] the record is not rewritten"
            );
            assert_eq!(current.crew_source, task.crew_source, "[{extra}]");
            assert!(fx.redraws(&task.id).is_empty(), "[{extra}]");
        }
    }
}

/// Criteria 2 and 3: the worked example's admission weighs Claude ×8 (capped
/// from about 25) and Codex ×1, names usage pressure in its source, and draws
/// a pool assignment from the whole pool. A changed crew is persisted under
/// the task's unchanged `pool:hard` source with a `crew_redrawn` entry.
fn usage_pressure_weighs_the_admission_draw_and_redraws_a_pool_assignment() {
    substitute_worker();
    let fx = Fixture::new(&config(r#"["sol"]"#, "[]", ""));
    let task = fx.task("pool.txt", TaskComplexity::Hard, None, &[]);
    assert_eq!(task.crew.as_deref(), Some("sol"));
    assert_eq!(task.crew_source.as_deref(), Some("pool:hard"));
    fx.configure(&config(POOL, "[]", "crew_selection = \"usage_pressure\""));
    read_example_week(&fx);

    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    let selection = &admitted["crew_selection"];
    assert_eq!(
        selection["source"], "workflow.hard_complexity_crews; usage pressure",
        "{admitted}"
    );
    let pool = selection["eligible_pool"].as_array().unwrap();
    let member = |name: &str| {
        pool.iter()
            .find(|member| member["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {admitted}"))
    };
    for (name, weight, effective) in [
        ("opus", 50, 400.0),
        ("sonnet", 25, 200.0),
        ("sol", 25, 25.0),
    ] {
        assert_eq!(member(name)["weight"], weight, "{admitted}");
        assert_eq!(member(name)["effective_weight"], effective, "{admitted}");
    }
    assert!(
        member("opus")["pressure"].as_f64().unwrap() > 25.0,
        "{admitted}"
    );
    assert!(
        (member("sol")["pressure"].as_f64().unwrap() - 1.0).abs() < 1e-3,
        "{admitted}"
    );

    let drawn = admitted["crew"].as_str().unwrap();
    let current = fx.runtime().get_task(&task.id).unwrap();
    assert_eq!(
        current.crew.as_deref(),
        Some(drawn),
        "the draw is persisted"
    );
    assert_eq!(current.crew_source.as_deref(), Some("pool:hard"));
    let redraws = fx.redraws(&task.id);
    if drawn == "sol" {
        assert!(
            redraws.is_empty(),
            "an unchanged crew is not re-recorded: {redraws:?}"
        );
    } else {
        let [note] = redraws.as_slice() else {
            panic!("one crew_redrawn entry: {redraws:?}");
        };
        assert!(
            note.starts_with(&format!(
                "crew redrawn from pool:hard (`sol`) to pool:hard (`{drawn}`) at admission under usage pressure: `sol` ×1"
            )) && note.contains(&format!("`{drawn}` ×25.")) && note.contains("seven_day 10%"),
            "the entry names both crews and their pressures: {note}"
        );
    }
}

/// Criterion 3: a decisive draw moves a `pool:hard` and a
/// `purpose:baseline_red_repair` assignment off Codex, each keeping its own
/// source. An explicit task crew and an explicit run crew are never re-drawn,
/// and the run allowlist bounds the re-draw.
fn a_redraw_keeps_the_automatic_source_and_never_moves_an_explicit_crew() {
    substitute_worker();
    let fx = Fixture::new(&config(r#"["sol"]"#, r#"["sol"]"#, ""));
    let pooled = fx.task("pooled.txt", TaskComplexity::Hard, None, &[]);
    let purpose = fx.task(
        "purpose.txt",
        TaskComplexity::Hard,
        None,
        &["baseline-red-repair"],
    );
    assert_eq!(
        purpose.crew_source.as_deref(),
        Some("purpose:baseline_red_repair")
    );
    let explicit = fx.task("explicit.txt", TaskComplexity::Hard, Some("sol"), &[]);
    assert_eq!(explicit.crew_source.as_deref(), Some("explicit"));
    fx.configure(&config(
        r#"["opus", "sol"]"#,
        r#"["opus", "sol"]"#,
        DECISIVE,
    ));
    fx.week("claude", 0.0, Duration::hours(6));
    fx.week("codex", 89.0, Duration::days(7));

    // The allowlist leaves only the assigned crew: nothing changes.
    let admitted = fx.admit(json!({ "task_ids": [pooled.id], "allowed_crews": ["sol"] }));
    assert_eq!(admitted["crew"], "sol", "{admitted}");
    assert!(fx.redraws(&pooled.id).is_empty());

    for (task, source) in [
        (&pooled, "pool:hard"),
        (&purpose, "purpose:baseline_red_repair"),
    ] {
        let admitted = fx.admit(json!({ "task_ids": [task.id] }));
        assert_eq!(admitted["crew"], "opus", "{admitted}");
        let current = fx.runtime().get_task(&task.id).unwrap();
        assert_eq!(current.crew.as_deref(), Some("opus"));
        assert_eq!(
            current.crew_source.as_deref(),
            Some(source),
            "the automatic source is kept"
        );
        let redraws = fx.redraws(&task.id);
        let [note] = redraws.as_slice() else {
            panic!("one crew_redrawn entry: {redraws:?}");
        };
        assert!(
            note.starts_with(&format!(
                "crew redrawn from {source} (`sol`) to {source} (`opus`) at admission under usage pressure:"
            )),
            "{note}"
        );
    }

    for _ in 0..3 {
        let admitted = fx.admit(json!({ "task_ids": [explicit.id] }));
        assert_eq!(
            admitted["crew"], "sol",
            "an explicit task crew stays: {admitted}"
        );
        assert_eq!(admitted["crew_selection"]["source"], "task.crew");
        let admitted = fx.admit(json!({
            "task_ids": [pooled.id], "crew": "sol",
            "crew_selection": {"task_id": pooled.id, "crew": "sol", "source": "explicit"},
        }));
        assert_eq!(
            admitted["crew"], "sol",
            "an explicit run crew stays: {admitted}"
        );
        assert_eq!(admitted["crew_selection"]["source"], "explicit");
    }
    let current = fx.runtime().get_task(&explicit.id).unwrap();
    assert_eq!(current.crew.as_deref(), Some("sol"));
    assert_eq!(current.crew_source.as_deref(), Some("explicit"));
    assert!(fx.redraws(&explicit.id).is_empty());
    assert_eq!(
        fx.runtime().get_task(&pooled.id).unwrap().crew.as_deref(),
        Some("opus"),
        "an explicit run crew does not rewrite the task"
    );
}

/// Criterion 4: creation and a complexity re-rate of a pool task draw with
/// usage pressure (`creation_crew_assignment` serves both).
fn a_complexity_rerate_of_a_pool_task_draws_with_usage_pressure() {
    let extra = format!("medium_complexity_crews = [\"sol\"]\n{DECISIVE}");
    let fx = Fixture::new(&config(r#"["opus", "sol"]"#, "[]", &extra));
    fx.week("claude", 0.0, Duration::hours(6));
    fx.week("codex", 89.0, Duration::days(7));

    let created = fx.task("created.txt", TaskComplexity::Hard, None, &[]);
    assert_eq!(
        created.crew.as_deref(),
        Some("opus"),
        "creation weighs pressure"
    );
    assert_eq!(created.crew_source.as_deref(), Some("pool:hard"));

    let task = fx.task("rerated.txt", TaskComplexity::Medium, None, &[]);
    assert_eq!(task.crew.as_deref(), Some("sol"));
    fx.runtime()
        .run_tool(
            "orbit.task.update",
            json!({"id": task.id, "complexity": "hard", "model": "codex"}),
        )
        .expect("complexity re-rate");
    let rerated = fx.runtime().get_task(&task.id).unwrap();
    assert_eq!(
        rerated.crew.as_deref(),
        Some("opus"),
        "the re-rate draw weighs pressure"
    );
    assert_eq!(rerated.crew_source.as_deref(), Some("pool:hard"));
    assert_eq!(
        fx.redraws(&task.id),
        [
            "crew redrawn from pool:medium (`sol`) to pool:hard (`opus`) after complexity changed to hard"
        ],
    );
}

/// Criterion 5: a parked member stays undrawable at any pressure, and a crew
/// at 95% weekly use with six days left draws at the ×1/8 floor and is gated
/// only by the existing threshold.
fn parked_members_stay_out_and_low_pressure_floors_without_gating() {
    substitute_worker();
    let fx = Fixture::new(&config("[]", "[]", ""));
    let task = fx.task("floor.txt", TaskComplexity::Hard, None, &[]);
    assert_eq!(task.crew_source.as_deref(), Some("default"));

    fx.configure(&config(r#"["opus:0", "sol:1"]"#, "[]", DECISIVE));
    fx.week("claude", 0.0, Duration::hours(6));
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    assert_eq!(admitted["crew"], "sol", "{admitted}");
    let pool = admitted["crew_selection"]["eligible_pool"]
        .as_array()
        .unwrap();
    assert_eq!(
        pool.len(),
        1,
        "the parked member holds no ticket: {admitted}"
    );
    assert_eq!(pool[0]["name"], "sol");

    fx.week("claude", 95.0, Duration::days(6));
    let pool = r#"["opus:1", "sol:1"]"#;
    fx.configure(&config(
        pool,
        "[]",
        "crew_selection = \"usage_pressure\"\nprovider_limit_max_used_pct = 96",
    ));
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    let selection = &admitted["crew_selection"];
    assert_eq!(
        selection["source"], "workflow.hard_complexity_crews; usage pressure",
        "below the threshold the crew is weighed, not gated: {admitted}"
    );
    let opus = selection["eligible_pool"]
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["name"] == "opus")
        .cloned()
        .unwrap_or_else(|| panic!("opus missing: {admitted}"));
    assert_eq!(opus["effective_weight"], 0.125, "{admitted}");
    assert!(opus["pressure"].as_f64().unwrap() < 0.125, "{admitted}");

    fx.configure(&config(pool, "[]", "crew_selection = \"usage_pressure\""));
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    assert_eq!(admitted["crew"], "sol", "{admitted}");
    let source = admitted["crew_selection"]["source"].as_str().unwrap();
    assert!(
        source
            .starts_with("workflow.hard_complexity_crews; provider limit: claude seven_day at 95%")
            && source.contains("crews opus skipped"),
        "the default 90% threshold still gates: {source}"
    );
}

crate::scenario_table!(
    usage_pressure_draw_scenarios,
    static_selection_draws_and_records_as_before,
    usage_pressure_weighs_the_admission_draw_and_redraws_a_pool_assignment,
    a_redraw_keeps_the_automatic_source_and_never_moves_an_explicit_crew,
    a_complexity_rerate_of_a_pool_task_draws_with_usage_pressure,
    parked_members_stay_out_and_low_pressure_floors_without_gating
);
