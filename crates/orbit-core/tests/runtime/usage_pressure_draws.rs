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
//!
//! `workflow.crew_performance` multiplies the same draws by each crew's
//! performance factor, read from a crew-performance snapshot seeded into the
//! workspace store the way the hourly refresh writes it.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    OrbitRuntime, ShipMode, Task, TaskComplexity, TaskStatus, WorkspaceRuntimeBinding,
};
use orbit_store::contracts::{
    CrewPerformanceCell, CrewPerformanceSnapshot, CrewPerformanceStoreBackend as _,
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

/// The crew-performance scope's worked example: a low pool of three crews,
/// `review` tasks tagged `review`, and `extra` under `[workflow]`.
fn performance_config(pool: &str, extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"luna\"\nlow_complexity_crews = {pool}\n{extra}\n\n\
         [[workflow.crew_performance_types]]\nname = \"review\"\ntags = [\"review\"]\n\n\
         [crews.haiku]\nprovider = \"claude\"\nmodel = \"haiku-model\"\n\n\
         [crews.luna]\nprovider = \"codex\"\nmodel = \"luna-model\"\n\n\
         [crews.gemini-flash]\nprovider = \"gemini\"\nmodel = \"flash-model\"\n\n\
         [review]\nbefore_pr = false\n"
    )
}

const EXAMPLE_POOL: &str = r#"["haiku:40", "luna:30", "gemini-flash:30"]"#;

/// A `low` cell of the snapshot: crew, model, type, `n`, score, crew mean.
type Cell<'a> = (&'a str, &'a str, &'a str, u64, f64, f64);

/// The worked example's cells: review scores 115/108/90 over tier means
/// 114/98/92, and an `other` cell per crew so each has a tier mean.
const EXAMPLE_CELLS: [Cell<'static>; 6] = [
    ("haiku", "haiku-model", "review", 40, 115.0, 114.0),
    ("haiku", "haiku-model", "other", 80, 113.5, 114.0),
    ("luna", "luna-model", "review", 40, 108.0, 98.0),
    ("luna", "luna-model", "other", 80, 93.0, 98.0),
    ("gemini-flash", "flash-model", "review", 40, 90.0, 92.0),
    ("gemini-flash", "flash-model", "other", 80, 93.0, 92.0),
];

impl Fixture {
    /// Store a snapshot of `cells`, computed `age` ago, built from the
    /// current type list unless `types_hash` names another.
    fn snapshot(&self, cells: &[Cell<'_>], age: Duration, types_hash: Option<&str>) {
        let runtime = self.runtime();
        let current = orbit_config::crew_performance_types_hash(runtime.crew_performance_types());
        runtime
            .sqlite_store()
            .unwrap()
            .replace_crew_performance_snapshot(&CrewPerformanceSnapshot {
                workspace_id: runtime.workspace_id().unwrap(),
                computed_at: Utc::now() - age,
                window_days: 30,
                types_hash: types_hash.map_or(current, ToOwned::to_owned),
                types_json: "[]".into(),
                cells: cells
                    .iter()
                    .map(
                        |(crew, model, task_type, n, score, crew_mean)| CrewPerformanceCell {
                            crew: (*crew).into(),
                            model: (*model).into(),
                            tier: "low".into(),
                            task_type: (*task_type).into(),
                            n: *n,
                            raw: *score,
                            crew_mean: *crew_mean,
                            score: *score,
                        },
                    )
                    .collect(),
            })
            .unwrap();
    }
}

/// `name`'s entry in an admitted run's recorded eligible pool.
fn pool_member<'a>(admitted: &'a Value, name: &str) -> &'a Value {
    admitted["crew_selection"]["eligible_pool"]
        .as_array()
        .unwrap_or_else(|| panic!("missing eligible pool: {admitted}"))
        .iter()
        .find(|member| member["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {admitted}"))
}

/// Assert each member's recorded performance factor to two places, and that
/// it drew on `weight × factor` tickets.
fn assert_factors(admitted: &Value, expected: &[(&str, u32, f64)]) {
    for (name, weight, factor) in expected {
        let member = pool_member(admitted, name);
        let recorded = member["performance"]["factor"]
            .as_f64()
            .unwrap_or_else(|| panic!("{name} has no factor: {admitted}"));
        assert!(
            (recorded - factor).abs() < 0.005,
            "{name} factor {recorded} is not ×{factor}: {admitted}"
        );
        assert_eq!(member["weight"], *weight, "{admitted}");
        let effective = (f64::from(*weight) * recorded * 1000.0).round() / 1000.0;
        assert_eq!(member["effective_weight"], effective, "{admitted}");
    }
}

/// Criteria 1 and 2: off records the configured weights exactly as a static
/// draw does, whatever the snapshot holds. The worked example's admission of
/// a `review` task records ×1.57/×1.27/×0.74 under `absolute` and
/// ×1.03/×1.35/×0.94 under `comparative`, with no pressure, and an assigned
/// pool member is never re-drawn by performance alone.
fn crew_performance_weighs_the_worked_example_and_off_draws_as_before() {
    substitute_worker();
    let fx = Fixture::new(&performance_config(r#"["luna"]"#, ""));
    let assigned = fx.task("assigned.txt", TaskComplexity::Low, None, &["review"]);
    assert_eq!(assigned.crew_source.as_deref(), Some("pool:low"));
    fx.configure(&performance_config("[]", ""));
    let task = fx.task("review.txt", TaskComplexity::Low, None, &["review"]);
    assert_eq!(task.crew_source.as_deref(), Some("default"));
    fx.configure(&performance_config(EXAMPLE_POOL, ""));
    fx.snapshot(&EXAMPLE_CELLS, Duration::hours(1), None);

    for extra in [
        "",
        "crew_performance = \"off\"",
        "crew_performance = \"absolute\"\ncrew_performance_strength = 0",
    ] {
        fx.configure(&performance_config(EXAMPLE_POOL, extra));
        let admitted = fx.admit(json!({ "task_ids": [task.id] }));
        assert_eq!(
            admitted["crew_selection"]["eligible_pool"],
            json!([
                {"name": "gemini-flash", "weight": 30},
                {"name": "haiku", "weight": 40},
                {"name": "luna", "weight": 30},
            ]),
            "[{extra}] configured weights, no factor fields: {admitted}"
        );
    }

    fx.configure(&performance_config(
        EXAMPLE_POOL,
        "crew_performance = \"absolute\"",
    ));
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    assert_eq!(
        admitted["crew_selection"]["source"], "workflow.low_complexity_crews",
        "performance alone does not rename the source: {admitted}"
    );
    assert_factors(
        &admitted,
        &[
            ("haiku", 40, 1.57),
            ("luna", 30, 1.27),
            ("gemini-flash", 30, 0.74),
        ],
    );
    let haiku = &pool_member(&admitted, "haiku")["performance"];
    assert_eq!(haiku["mode"], "absolute", "{admitted}");
    assert_eq!(haiku["type"], "review", "{admitted}");
    assert_eq!(haiku["tier"], "low", "{admitted}");
    assert_eq!(haiku["score"], 115.0, "{admitted}");
    assert_eq!(haiku["crew_mean"], 114.0, "{admitted}");
    assert_eq!(haiku["n"], 40, "{admitted}");
    assert!(haiku["computed_at"].is_string(), "{admitted}");
    assert!(
        pool_member(&admitted, "haiku").get("pressure").is_none(),
        "no pressure without usage_pressure: {admitted}"
    );

    fx.configure(&performance_config(
        EXAMPLE_POOL,
        "crew_performance = \"comparative\"",
    ));
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    assert_factors(
        &admitted,
        &[
            ("haiku", 40, 1.03),
            ("luna", 30, 1.35),
            ("gemini-flash", 30, 0.94),
        ],
    );

    // The assigned member stays: only usage pressure re-draws at admission.
    let admitted = fx.admit(json!({ "task_ids": [assigned.id] }));
    assert_eq!(
        admitted["crew_selection"]["eligible_pool"],
        json!([{"name": "luna", "weight": 1}]),
        "{admitted}"
    );
    assert_eq!(
        fx.runtime().get_task(&assigned.id).unwrap().crew.as_deref(),
        Some("luna")
    );
    assert!(fx.redraws(&assigned.id).is_empty());
}

/// Criterion 3: with usage pressure too, each member draws on the product of
/// both factors and records both, and a re-draw's history names both.
fn crew_performance_composes_with_usage_pressure() {
    substitute_worker();
    let fx = Fixture::new(&performance_config(r#"["gemini-flash"]"#, ""));
    let task = fx.task("both.txt", TaskComplexity::Low, None, &["review"]);
    assert_eq!(task.crew.as_deref(), Some("gemini-flash"));
    fx.configure(&performance_config(
        EXAMPLE_POOL,
        "crew_selection = \"usage_pressure\"\ncrew_performance = \"absolute\"",
    ));
    fx.snapshot(&EXAMPLE_CELLS, Duration::hours(1), None);
    fx.week("claude", 10.0, Duration::hours(6));
    fx.week("codex", 0.0, Duration::days(7));

    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    assert_eq!(
        admitted["crew_selection"]["source"], "workflow.low_complexity_crews; usage pressure",
        "{admitted}"
    );
    // Claude's pressure (about 25) caps at ×8; Codex reads 1.
    for (name, pressure_factor, performance_factor) in
        [("haiku", 8.0, 1.568_312), ("luna", 1.0, 1.271_249)]
    {
        let member = pool_member(&admitted, name);
        assert!(member["pressure"].as_f64().is_some(), "{admitted}");
        let recorded = member["performance"]["factor"].as_f64().unwrap();
        assert!((recorded - performance_factor).abs() < 1e-5, "{admitted}");
        let weight = member["weight"].as_u64().unwrap() as f64;
        let effective = (weight * pressure_factor * recorded * 1000.0).round() / 1000.0;
        assert_eq!(member["effective_weight"], effective, "{name}: {admitted}");
    }
    let drawn = admitted["crew"].as_str().unwrap();
    if drawn != "gemini-flash" {
        let redraws = fx.redraws(&task.id);
        let [note] = redraws.as_slice() else {
            panic!("one crew_redrawn entry: {redraws:?}");
        };
        assert!(
            note.contains("under usage pressure:")
                && note.contains(
                    "; crew performance absolute: `gemini-flash` ×0.74 (review score 90.0"
                ),
            "the entry names both factors: {note}"
        );
    }
}

/// Criteria 4 and 5: a crew with no cell for the type reads its crew mean,
/// a crew with no evidence reads 100, factors clamp to `[1/cap, cap]`, a
/// missing, stale or other-list snapshot gives every factor 1 with its
/// reason and draws on the configured weights, a parked member holds no
/// ticket, and creation draws weigh performance.
fn crew_performance_fallbacks_clamps_and_unusable_snapshots() {
    substitute_worker();
    let fx = Fixture::new(&performance_config("[]", ""));
    let task = fx.task("fallback.txt", TaskComplexity::Low, None, &["review"]);
    let absolute = "crew_performance = \"absolute\"";

    // Haiku has only an `other` cell (crew mean 120), luna has none; the
    // `[1/cap, cap]` clamp at cap 1.5 holds haiku's ×1.82 to ×1.5.
    fx.configure(&performance_config(
        EXAMPLE_POOL,
        &format!("{absolute}\ncrew_performance_cap = 1.5"),
    ));
    fx.snapshot(
        &[("haiku", "haiku-model", "other", 50, 125.0, 120.0)],
        Duration::hours(1),
        None,
    );
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    let haiku = &pool_member(&admitted, "haiku")["performance"];
    assert_eq!(
        haiku["score"], 120.0,
        "a missing cell reads its crew mean: {admitted}"
    );
    assert_eq!(haiku["evidence"], "crew_mean", "{admitted}");
    assert_eq!(haiku["factor"], 1.5, "clamped to the cap: {admitted}");
    let luna = &pool_member(&admitted, "luna")["performance"];
    assert_eq!(
        luna["score"], 100.0,
        "a crew with no evidence reads 100: {admitted}"
    );
    assert_eq!(luna["evidence"], "none", "{admitted}");
    assert_eq!(luna["factor"], 1.0, "{admitted}");

    // A low score floors at 1/cap.
    fx.snapshot(
        &[("luna", "luna-model", "review", 50, 40.0, 60.0)],
        Duration::hours(1),
        None,
    );
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    let floor = pool_member(&admitted, "luna")["performance"]["factor"]
        .as_f64()
        .unwrap();
    assert!((floor - 1.0 / 1.5).abs() < 1e-9, "{admitted}");

    // Unusable snapshots: factor 1, the reason, and the configured weights.
    fx.configure(&performance_config(EXAMPLE_POOL, absolute));
    for (age, hash, reason) in [
        (Duration::hours(25), None, "stale"),
        (
            Duration::hours(1),
            Some("another-list"),
            "type_list_changed",
        ),
    ] {
        fx.snapshot(&EXAMPLE_CELLS, age, hash);
        let admitted = fx.admit(json!({ "task_ids": [task.id] }));
        for name in ["haiku", "luna", "gemini-flash"] {
            let member = pool_member(&admitted, name);
            assert_eq!(member["performance"]["factor"], 1.0, "{admitted}");
            assert_eq!(member["performance"]["unusable"], reason, "{admitted}");
            assert!(member.get("effective_weight").is_none(), "{admitted}");
        }
    }
    let empty = Fixture::new(&performance_config(EXAMPLE_POOL, absolute));
    let fresh = empty.task("missing.txt", TaskComplexity::Low, None, &["review"]);
    empty.configure(&performance_config("[]", ""));
    let unpooled = empty.task("unpooled.txt", TaskComplexity::Low, None, &["review"]);
    empty.configure(&performance_config(EXAMPLE_POOL, absolute));
    let admitted = empty.admit(json!({ "task_ids": [unpooled.id] }));
    assert_eq!(
        pool_member(&admitted, "haiku")["performance"]["unusable"],
        "missing",
        "{admitted}"
    );
    assert_eq!(fresh.crew_source.as_deref(), Some("pool:low"));

    // A parked member holds no ticket, and a decisive factor fixes the
    // creation draw: haiku's +15 against luna's −10 at k = 100.
    fx.configure(&performance_config(
        r#"["haiku:1", "luna:1", "gemini-flash:0"]"#,
        &format!("{absolute}\ncrew_performance_strength = 100\ncrew_performance_cap = 1000000"),
    ));
    fx.snapshot(
        &[
            ("haiku", "haiku-model", "review", 50, 115.0, 110.0),
            ("luna", "luna-model", "review", 50, 90.0, 100.0),
        ],
        Duration::hours(1),
        None,
    );
    for name in ["c1.txt", "c2.txt", "c3.txt"] {
        let created = fx.task(name, TaskComplexity::Low, None, &["review"]);
        assert_eq!(
            created.crew.as_deref(),
            Some("haiku"),
            "creation weighs performance"
        );
        assert_eq!(created.crew_source.as_deref(), Some("pool:low"));
    }
    let admitted = fx.admit(json!({ "task_ids": [task.id] }));
    let pool = admitted["crew_selection"]["eligible_pool"]
        .as_array()
        .unwrap();
    assert!(
        pool.iter().all(|member| member["name"] != "gemini-flash"),
        "the parked member holds no ticket: {admitted}"
    );
}

crate::scenario_table!(
    usage_pressure_draw_scenarios,
    static_selection_draws_and_records_as_before,
    usage_pressure_weighs_the_admission_draw_and_redraws_a_pool_assignment,
    a_redraw_keeps_the_automatic_source_and_never_moves_an_explicit_crew,
    a_complexity_rerate_of_a_pool_task_draws_with_usage_pressure,
    parked_members_stay_out_and_low_pressure_floors_without_gating,
    crew_performance_weighs_the_worked_example_and_off_draws_as_before,
    crew_performance_composes_with_usage_pressure,
    crew_performance_fallbacks_clamps_and_unusable_snapshots
);
