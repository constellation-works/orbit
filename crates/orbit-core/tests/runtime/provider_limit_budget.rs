//! [ORB-14699] An operator-declared budget gates a provider that reports no
//! usage, from the spend this host's invocation ledger holds for it.
//!
//! Invocations are recorded the way a run's telemetry records them, then moved
//! to a known age. Gating is read from `orbit run readiness` and the shared
//! provider-limit view.

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, ShipMode, TaskComplexity, TaskStatus, WorkspaceRuntimeBinding};
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::telemetry::{
    InvocationTrace, ProviderLimitObservation, ProviderLimitSource, TokenUsage,
};
use serde_json::Value;
use tempfile::TempDir;

use super::dispatch_admission::isolated;

/// A grok and an antigravity crew as the hard pool, with `workflow_extra`
/// under `[workflow]`.
fn config(workflow_extra: &str) -> String {
    format!(
        "[workflow]\ndefault_crew = \"sol\"\nhard_complexity_crews = [\"groky\"]\n\
         {workflow_extra}\n\n\
         [crews.groky]\nprovider = \"grok\"\nmodel = \"grok-4.7\"\n\n\
         [crews.gravity]\nprovider = \"antigravity\"\nmodel = \"gemini-3.8-flash-high\"\n\n\
         [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
         [review]\nbefore_pr = false\n"
    )
}

struct Fixture {
    _root: TempDir,
    runtime: OrbitRuntime,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let root = TempDir::new().unwrap();
        std::fs::create_dir_all(root.path().join("global/resources/jobs")).unwrap();
        std::fs::create_dir_all(root.path().join("repo/.orbit")).unwrap();
        std::fs::write(root.path().join("repo/.orbit/config.toml"), config).unwrap();
        let repo = root.path().join("repo");
        let runtime = OrbitRuntime::from_roots_with_binding(
            &root.path().join("global"),
            &repo.join(".orbit"),
            WorkspaceRuntimeBinding {
                logical_workspace_id: "ws_provider_budget".into(),
                task_partition_id: "ws_provider_budget".into(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: repo,
                ship_mode: ShipMode::Local,
                base_branch: Some("main".into()),
            },
        )
        .unwrap();
        Self {
            _root: root,
            runtime,
        }
    }

    /// Record an invocation of `provider` and move it to `age` ago.
    fn spend(
        &self,
        provider: &str,
        agent: &str,
        tokens: (u64, u64),
        cost: Option<f64>,
        age: Duration,
    ) -> DateTime<Utc> {
        self.runtime
            .insert_invocation_trace_record(&InvocationInsertParams {
                job_run_id: "fixture-run".into(),
                activity_id: "implement_one".into(),
                agent: agent.into(),
                provider: Some(provider.into()),
                model: None,
                task_ids: Vec::new(),
                trace: InvocationTrace {
                    usage: TokenUsage {
                        input: tokens.0,
                        output: tokens.1,
                        ..TokenUsage::default()
                    },
                    provider_cost_usd: cost,
                    ..InvocationTrace::default()
                },
            })
            .unwrap();
        let at = Utc::now() - age;
        rusqlite::Connection::open(self.runtime.global_root().join("orbit.db"))
            .unwrap()
            .execute(
                "UPDATE invocations SET ts = ?1 WHERE id = (SELECT MAX(id) FROM invocations)",
                [at.to_rfc3339()],
            )
            .unwrap();
        at
    }

    fn grok(&self, cost: f64, age: Duration) -> DateTime<Utc> {
        self.spend("grok", "grok", (1_000, 100), Some(cost), age)
    }

    /// The live readings of `provider` in the shared view.
    fn readings(&self, provider: &str) -> Vec<orbit_core::application::task::ProviderLimitReading> {
        self.runtime
            .provider_limits_view(Utc::now())
            .provider_readings(provider)
            .cloned()
            .collect()
    }

    fn task(&self) -> String {
        self.runtime
            .add_task(TaskAddParams {
                title: "Provider budget fixture".into(),
                description: "A task whose crew's provider is near its budget.".into(),
                acceptance_criteria: vec!["Delivered.".into()],
                plan: "1. Deliver it.".into(),
                complexity: TaskComplexity::Hard,
                context_files: vec!["dir:.".into()],
                task_type: Some(orbit_core::TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// `task`'s entry in `orbit run readiness --json`.
    fn readiness(&self, task: &str) -> Value {
        let readiness = self
            .runtime
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

    fn stored_reading(
        &self,
        source: ProviderLimitSource,
        exhausted: bool,
        used_percent: Option<f64>,
        resets_at: DateTime<Utc>,
    ) {
        self.runtime
            .record_provider_limit(&ProviderLimitObservation {
                provider: "grok".into(),
                model: None,
                window: Some("session".into()),
                exhausted,
                source,
                resets_at: Some(resets_at),
                used_percent,
                window_minutes: None,
                gating: true,
                partial: false,
                observed_at: Utc::now(),
                run_id: None,
                crew: None,
                detail: String::new(),
            })
            .unwrap();
    }
}

#[test]
fn spend_at_the_threshold_gates_until_the_oldest_spend_ages_out() {
    if !isolated(
        "provider_limit_budget::spend_at_the_threshold_gates_until_the_oldest_spend_ages_out",
    ) {
        return;
    }
    let fx = Fixture::new(&config("provider_limit_budgets = [\"grok:30usd/5h\"]"));
    let task = fx.task();
    // $27 of $30 in the trailing 5h is 90%; the $50 outside the window and the
    // other provider's spend count for nothing.
    let oldest = fx.grok(12.0, Duration::hours(4));
    fx.grok(15.0, Duration::hours(2));
    fx.grok(50.0, Duration::hours(6));
    fx.spend("codex", "codex", (1, 1), Some(80.0), Duration::hours(1));

    let [reading] = fx.readings("grok").try_into().unwrap();
    assert_eq!(reading.source, ProviderLimitSource::Ledger);
    assert_eq!(reading.used_percent, Some(90.0));
    assert!(
        reading.gated && reading.gating && !reading.partial,
        "{reading:?}"
    );
    assert_eq!(reading.threshold, 90);
    assert_eq!(reading.crews, ["groky"]);
    assert_eq!(
        reading.resets_at,
        Some(oldest + Duration::hours(5)),
        "the reset is when the oldest qualifying spend leaves the window"
    );

    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], false, "{entry}");
    assert_eq!(entry["reason"], "provider_limit", "{entry}");
    let detail = entry["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("grok 5h budget at 90% (limit 90%) until ")
            && detail.ends_with("crews groky skipped"),
        "{detail}"
    );
}

#[test]
fn spend_below_the_threshold_gates_nothing() {
    if !isolated("provider_limit_budget::spend_below_the_threshold_gates_nothing") {
        return;
    }
    let fx = Fixture::new(&config("provider_limit_budgets = [\"grok:30usd/5h\"]"));
    let task = fx.task();
    fx.grok(12.0, Duration::hours(4));
    fx.grok(8.0, Duration::hours(2));

    let [reading] = fx.readings("grok").try_into().unwrap();
    assert!(
        reading
            .used_percent
            .is_some_and(|used| (used - 66.666).abs() < 0.01),
        "{reading:?}"
    );
    assert!(!reading.gated && reading.resets_at.is_none(), "{reading:?}");
    let entry = fx.readiness(&task);
    assert_eq!(entry["eligible"], true, "$20 of $30 is under 90%: {entry}");
}

#[test]
fn a_token_budget_counts_the_providers_tokens_and_resets_with_the_oldest() {
    if !isolated(
        "provider_limit_budget::a_token_budget_counts_the_providers_tokens_and_resets_with_the_oldest",
    ) {
        return;
    }
    let fx = Fixture::new(&config(
        "provider_limit_budgets = [\"antigravity:1000tokens/1d\"]",
    ));
    // Antigravity rows carry the model family as their agent and no cost.
    let oldest = fx.spend(
        "antigravity",
        "gemini",
        (200, 100),
        None,
        Duration::hours(20),
    );
    fx.spend(
        "antigravity",
        "gemini",
        (400, 200),
        None,
        Duration::hours(2),
    );
    fx.spend("gemini", "gemini", (5_000, 5_000), None, Duration::hours(1));

    let [reading] = fx.readings("antigravity").try_into().unwrap();
    assert_eq!(reading.used_percent, Some(90.0));
    assert!(reading.gated, "{reading:?}");
    assert!(
        !reading.partial,
        "a token budget needs no cost: {reading:?}"
    );
    assert_eq!(reading.crews, ["gravity"]);
    assert_eq!(
        reading.resets_at,
        Some(oldest + Duration::hours(24)),
        "the oldest 300 tokens age out first, leaving 60%"
    );
    assert!(fx.readings("gemini").is_empty());
}

#[test]
fn a_dollar_budget_marks_the_reading_partial_when_invocations_lack_a_cost() {
    if !isolated(
        "provider_limit_budget::a_dollar_budget_marks_the_reading_partial_when_invocations_lack_a_cost",
    ) {
        return;
    }
    let fx = Fixture::new(&config("provider_limit_budgets = [\"grok:30usd/5h\"]"));
    fx.grok(27.0, Duration::hours(1));
    let [reading] = fx.readings("grok").try_into().unwrap();
    assert!(!reading.partial, "{reading:?}");

    fx.spend("grok", "grok", (900_000, 1), None, Duration::hours(2));
    let [reading] = fx.readings("grok").try_into().unwrap();
    assert_eq!(
        reading.used_percent,
        Some(90.0),
        "uncosted spend counts for nothing"
    );
    assert!(reading.partial, "{reading:?}");
    let line = reading.describe(Utc::now());
    assert!(
        line.contains("partial: some invocations report no cost"),
        "{line}"
    );
}

#[test]
fn a_reported_reading_or_limit_failure_takes_precedence_over_the_ledger() {
    if !isolated(
        "provider_limit_budget::a_reported_reading_or_limit_failure_takes_precedence_over_the_ledger",
    ) {
        return;
    }
    let fx = Fixture::new(&config("provider_limit_budgets = [\"grok:30usd/5h\"]"));
    let task = fx.task();
    fx.grok(29.0, Duration::hours(1));
    assert_eq!(fx.readings("grok")[0].source, ProviderLimitSource::Ledger);

    // The provider's own low reading stands in for the ledger's 97%.
    fx.stored_reading(
        ProviderLimitSource::Event,
        false,
        Some(10.0),
        Utc::now() + Duration::hours(2),
    );
    let [reading] = fx.readings("grok").try_into().unwrap();
    assert_eq!(reading.source, ProviderLimitSource::Event, "{reading:?}");
    assert!(!reading.gated, "{reading:?}");
    assert_eq!(fx.readiness(&task)["eligible"], true);

    // A limit failure gates by its own reset, not the ledger's.
    let reset = Utc::now() + Duration::minutes(45);
    fx.stored_reading(ProviderLimitSource::Error, true, None, reset);
    let [reading] = fx.readings("grok").try_into().unwrap();
    assert_eq!(reading.source, ProviderLimitSource::Error, "{reading:?}");
    assert!(reading.gated, "{reading:?}");
    assert_eq!(fx.readiness(&task)["reason"], "provider_limit");

    // Once the failure's reading lapses the ledger speaks again.
    fx.stored_reading(
        ProviderLimitSource::Error,
        true,
        None,
        Utc::now() - Duration::seconds(1),
    );
    let [reading] = fx.readings("grok").try_into().unwrap();
    assert_eq!(reading.source, ProviderLimitSource::Ledger, "{reading:?}");
    assert!(reading.gated, "{reading:?}");
}
