//! [ORB-14697] Delivery admission skips crews whose provider this host reads
//! at or near its usage limit, and draws them again once the reading lapses.
//!
//! Readings are seeded into the host's provider-limit store the way a run's
//! telemetry records them. Readiness is `orbit run readiness --json`; a crew
//! selection is read from a replay's admitted run input, the same admission
//! every delivery submission runs.

use chrono::{DateTime, Duration, SubsecRound, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, ShipMode, TaskComplexity, TaskStatus, WorkspaceRuntimeBinding};
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{JobRunState, JobRunTrigger};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::dispatch_admission::isolated;

/// A Claude and a Codex crew, `pool` as the hard-complexity pool, and
/// `extra` under `[workflow]`.
fn config(pool: &str, extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\nhard_complexity_crews = {pool}\n{extra}\n\n\
         [crews.opus]\nprovider = \"claude\"\nmodel = \"opus-model\"\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
         [review]\nbefore_pr = false\n"
    )
}

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("global/resources/jobs")).unwrap();
        std::fs::create_dir_all(root.path().join("repo/.orbit")).unwrap();
        let fixture = Self { root };
        fixture.configure(config);
        fixture
    }

    fn configure(&self, config: &str) {
        std::fs::write(self.root.path().join("repo/.orbit/config.toml"), config).unwrap();
    }

    /// The runtime over the current configuration.
    fn runtime(&self) -> OrbitRuntime {
        let repo = self.root.path().join("repo");
        OrbitRuntime::from_roots_with_binding(
            &self.root.path().join("global"),
            &repo.join(".orbit"),
            WorkspaceRuntimeBinding {
                logical_workspace_id: "ws_provider_limit".into(),
                task_partition_id: "ws_provider_limit".into(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: repo,
                ship_mode: ShipMode::Local,
                base_branch: Some("main".into()),
            },
        )
        .unwrap()
    }

    /// An admissible hard backlog task, on `crew` when one is named.
    fn task(&self, crew: Option<&str>) -> String {
        self.runtime()
            .add_task(TaskAddParams {
                title: "Provider limit fixture".into(),
                description: "A task whose crew's provider is near its limit.".into(),
                acceptance_criteria: vec!["Delivered.".into()],
                plan: "1. Deliver it.".into(),
                complexity: TaskComplexity::Hard,
                context_files: vec!["dir:.".into()],
                task_type: Some(orbit_core::TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                crew: crew.map(ToOwned::to_owned),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// Record a Claude `five_hour` reading, observed now.
    fn read_claude(&self, used_percent: f64, exhausted: bool, resets_at: DateTime<Utc>) {
        self.runtime()
            .record_provider_limit(&ProviderLimitObservation {
                provider: "claude".into(),
                model: None,
                window: Some("five_hour".into()),
                exhausted,
                source: ProviderLimitSource::Event,
                resets_at: Some(resets_at),
                used_percent: Some(used_percent),
                window_minutes: Some(300),
                gating: true,
                observed_at: Utc::now(),
                run_id: None,
                crew: None,
                detail: String::new(),
            })
            .unwrap();
    }

    /// `task`'s entry in `orbit run readiness --json`.
    fn readiness(&self, task: &str) -> Value {
        let readiness = self
            .runtime()
            .workspace_auto_readiness(&[], None, 50, &[])
            .unwrap();
        readiness["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["task_id"] == task)
            .cloned()
            .unwrap_or_else(|| panic!("{task} missing from readiness: {readiness}"))
    }

    /// The run input a replay of a failed delivery of `task`, submitted with
    /// `input`, is admitted with.
    fn replay_admission(&self, input: Value) -> Value {
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

#[test]
fn a_task_waits_on_an_exhausted_provider_until_the_reset() {
    if !isolated("provider_limit_admission::a_task_waits_on_an_exhausted_provider_until_the_reset")
    {
        return;
    }
    let fx = Fixture::new(&config(r#"["opus"]"#, ""));
    let task = fx.task(None);
    // Whole seconds: the store keeps microseconds, but a Linux clock reads nanoseconds.
    let reset = (Utc::now() + Duration::hours(2)).trunc_subsecs(0);
    fx.read_claude(100.0, true, reset);

    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "provider_limit", "{entry}");
    assert_eq!(
        entry["detail"],
        format!(
            "claude five_hour at 100% (limit 90%) until {}; crews opus skipped",
            reset.to_rfc3339()
        ),
        "the detail names provider, window, used percent, threshold and reset: {entry}"
    );

    // Past the reset, the next reading no longer counts.
    fx.read_claude(100.0, true, Utc::now() - Duration::seconds(1));
    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], true, "{entry}");
}

#[test]
fn an_explicit_crew_waits_unless_the_policy_draws_from_its_pool() {
    if !isolated(
        "provider_limit_admission::an_explicit_crew_waits_unless_the_policy_draws_from_its_pool",
    ) {
        return;
    }
    substitute_worker();
    let pool = r#"["opus", "sol"]"#;
    let fx = Fixture::new(&config(pool, ""));
    let task = fx.task(Some("opus"));
    fx.read_claude(93.0, false, Utc::now() + Duration::hours(2));

    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "provider_limit", "{entry}");

    fx.configure(&config(pool, "provider_limit_explicit_crews = \"pool\""));
    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], true, "{entry}");
    let admitted = fx.replay_admission(json!({ "task_ids": [task] }));
    assert_eq!(admitted["crew"], "sol", "{admitted}");
    let source = admitted["crew_selection"]["source"].as_str().unwrap();
    assert!(
        source
            .starts_with("workflow.hard_complexity_crews; provider limit: claude five_hour at 93%")
            && source.ends_with("crews opus skipped"),
        "the selection names the limit: {source}"
    );

    // An explicit run crew is the operator's call: not gated, but recorded.
    let admitted = fx.replay_admission(json!({
        "task_ids": [task],
        "crew": "opus",
        "crew_selection": {"task_id": task, "crew": "opus", "source": "explicit"},
    }));
    assert_eq!(admitted["crew"], "opus", "{admitted}");
    assert_eq!(admitted["crew_selection"]["source"], "explicit");
    let limit = admitted["crew_selection"]["provider_limit"]
        .as_str()
        .unwrap_or_default();
    assert!(
        limit.starts_with("claude five_hour at 93% (limit 90%)"),
        "{admitted}"
    );
}

#[test]
fn an_override_at_one_hundred_gates_only_an_exhausted_window() {
    if !isolated(
        "provider_limit_admission::an_override_at_one_hundred_gates_only_an_exhausted_window",
    ) {
        return;
    }
    let fx = Fixture::new(&config(
        r#"["opus"]"#,
        "provider_limit_overrides = [\"claude:100\"]",
    ));
    let task = fx.task(None);
    fx.read_claude(99.0, false, Utc::now() + Duration::hours(2));
    let entry = fx.readiness(&task);
    assert_eq!(
        entry["eligible"], true,
        "99% is under a 100% override: {entry}"
    );

    fx.read_claude(100.0, true, Utc::now() + Duration::hours(2));
    let entry = fx.readiness(&task);
    assert_eq!(entry["reason"], "provider_limit", "{entry}");
}
