use chrono::{Duration, Utc};
use orbit_store::scoreboard_summary::{ORCHESTRATION_SCHEMA_VERSION, ScoreboardWindow};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use tempfile::tempdir;

use crate::application::task::TaskAddParams;
use crate::runtime::engine::OrchestratorInvocationMetricsBucket;
use crate::{InvocationInsertParams, OrbitRuntime, OrchestratorMetricsBucketKind};

const PRICED_MODEL: &str = "claude-opus-4-7";
const UNPRICED_MODEL: &str = "unpriced-model";

fn accounting_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    std::fs::write(
        workspace_root.join("config.toml"),
        r#"
[workflow]
default_crew = "alpha"

[crews.alpha]
model = "alpha-implement"
provider = "codex"
backend = "cli"

[crews.beta]
model = "beta-implement"
provider = "claude"
backend = "cli"
"#,
    )
    .expect("write config");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build runtime");
    (root, runtime)
}

fn add_task(runtime: &OrbitRuntime, title: &str, orchestrator: Option<&str>) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "accounting fixture".to_string(),
            orchestrator: orchestrator.map(ToOwned::to_owned),
            ..TaskAddParams::default()
        })
        .expect("add task")
        .id
        .to_string()
}

fn insert(
    runtime: &OrbitRuntime,
    index: usize,
    task_ids: Vec<String>,
    model: &str,
    provider_cost_usd: Option<f64>,
) {
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: format!("jrun-accounting-{index}"),
            activity_id: "agent_implement".to_string(),
            agent: "codex".to_string(),
            model: Some(model.to_string()),
            task_ids,
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 10,
                    cache_read: 2,
                    cache_create: 3,
                    cache_create_1h: 4,
                    output: 5,
                },
                provider_cost_usd,
                ..InvocationTrace::default()
            },
        })
        .expect("insert invocation");
}

fn bucket<'a>(
    buckets: &'a [OrchestratorInvocationMetricsBucket],
    kind: OrchestratorMetricsBucketKind,
    orchestrator: Option<&str>,
) -> &'a OrchestratorInvocationMetricsBucket {
    buckets
        .iter()
        .find(|bucket| bucket.kind == kind && bucket.orchestrator.as_deref() == orchestrator)
        .expect("metrics bucket")
}

#[test]
fn orchestrator_accounting_classifies_conservatively_and_reconciles_every_population() {
    let (_root, runtime) = accounting_runtime();
    let alpha_one = add_task(&runtime, "alpha one", Some("alpha"));
    let alpha_two = add_task(&runtime, "alpha two", Some("alpha"));
    let beta = add_task(&runtime, "beta", Some("beta"));
    let unattributed = add_task(&runtime, "unattributed", None);

    insert(
        &runtime,
        0,
        vec![alpha_one.clone(), alpha_one.clone()],
        PRICED_MODEL,
        Some(1.0),
    );
    insert(
        &runtime,
        1,
        vec![alpha_one.clone(), alpha_two],
        PRICED_MODEL,
        None,
    );
    insert(
        &runtime,
        2,
        vec![alpha_one.clone(), beta],
        UNPRICED_MODEL,
        Some(2.0),
    );
    insert(&runtime, 3, Vec::new(), UNPRICED_MODEL, None);
    insert(
        &runtime,
        4,
        vec![unattributed.clone()],
        PRICED_MODEL,
        Some(3.0),
    );
    insert(
        &runtime,
        5,
        vec![alpha_one.clone(), unattributed],
        PRICED_MODEL,
        Some(-1.0),
    );
    insert(
        &runtime,
        6,
        vec!["ORB-MISSING".to_string()],
        PRICED_MODEL,
        Some(4.0),
    );
    insert(
        &runtime,
        7,
        vec![alpha_one, "ORB-MISSING-TOO".to_string()],
        UNPRICED_MODEL,
        None,
    );

    let metrics = runtime
        .orchestrator_invocation_metrics(None, None)
        .expect("orchestrator metrics");
    assert!(metrics.until <= metrics.as_of);
    assert_eq!(metrics.buckets.len(), 4);
    assert_eq!(
        bucket(
            &metrics.buckets,
            OrchestratorMetricsBucketKind::Orchestrator,
            Some("alpha")
        )
        .invocation_count,
        2,
        "duplicate and same-owner task links charge alpha once per invocation"
    );
    assert_eq!(
        bucket(
            &metrics.buckets,
            OrchestratorMetricsBucketKind::Shared,
            None
        )
        .invocation_count,
        1
    );
    assert_eq!(
        bucket(
            &metrics.buckets,
            OrchestratorMetricsBucketKind::Unattributed,
            None
        )
        .invocation_count,
        3,
        "no tasks and partial unattribution stay unattributed"
    );
    assert_eq!(
        bucket(
            &metrics.buckets,
            OrchestratorMetricsBucketKind::Missing,
            None
        )
        .invocation_count,
        2,
        "a missing task takes precedence over a known owner"
    );

    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.invocation_count)
            .sum::<u64>(),
        8
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.input_tokens)
            .sum::<u64>(),
        80
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.cache_read_tokens)
            .sum::<u64>(),
        16
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.cache_create_tokens)
            .sum::<u64>(),
        24
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.cache_create_1h_tokens)
            .sum::<u64>(),
        32
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.output_tokens)
            .sum::<u64>(),
        40
    );
    let normalized = &metrics.normalized_tokens;
    assert_eq!(normalized.invocation_count, 8);
    assert_eq!(normalized.covered_invocation_count, 5);
    assert_eq!(normalized.unknown_input_basis_or_model_count, 3);
    assert_eq!(normalized.uncached_input_tokens, 50);
    assert_eq!(normalized.cache_read_tokens, 10);
    assert_eq!(normalized.cache_create_tokens, 15);
    assert_eq!(normalized.cache_create_1h_tokens, 20);
    assert_eq!(normalized.output_tokens, 25);
    assert_eq!(normalized.normalized_token_total, 120);
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.provider_cost_count + bucket.missing_provider_count)
            .sum::<u64>(),
        8
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.derived_cost_count + bucket.unpriced_derived_count)
            .sum::<u64>(),
        8
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.comparable_cost_count)
            .sum::<u64>(),
        3,
        "only invocations with both valid costs enter comparable sums"
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.provider_cost_count)
            .sum::<u64>(),
        4,
        "provider-only and both-cost rows contribute provider cost"
    );
    assert_eq!(
        metrics
            .buckets
            .iter()
            .map(|bucket| bucket.derived_cost_count)
            .sum::<u64>(),
        5,
        "derived-only and both-cost rows contribute derived cost"
    );
    for bucket in &metrics.buckets {
        assert_eq!(
            bucket.comparable_cost_delta_usd,
            bucket.comparable_provider_cost_usd - bucket.comparable_derived_cost_usd
        );
    }

    let scoreboard = runtime
        .generate_scoreboard_summary(Some(ScoreboardWindow::Hour))
        .expect("generate bounded scoreboard");
    let orchestration = scoreboard
        .orchestration
        .expect("scoreboard preserves orchestration projection");
    assert_eq!(orchestration.schema_version, ORCHESTRATION_SCHEMA_VERSION);
    assert_eq!(orchestration.scope, "managed_execution");
    assert!(orchestration.until <= orchestration.as_of);
    assert!(orchestration.since.is_some(), "bounded window is preserved");
    assert_eq!(orchestration.buckets.len(), 4);
    assert_eq!(orchestration.normalized_tokens.normalized_token_total, 120);
    assert!(orchestration.buckets.iter().any(|bucket| bucket.kind
        == OrchestratorMetricsBucketKind::Orchestrator
        && bucket.orchestrator.as_deref() == Some("alpha")));
    assert!(
        orchestration
            .buckets
            .iter()
            .any(|bucket| bucket.kind == OrchestratorMetricsBucketKind::Shared)
    );
    assert!(
        orchestration
            .buckets
            .iter()
            .any(|bucket| bucket.kind == OrchestratorMetricsBucketKind::Unattributed)
    );
    assert!(
        orchestration
            .buckets
            .iter()
            .any(|bucket| bucket.kind == OrchestratorMetricsBucketKind::Missing)
    );
}

#[test]
fn orchestrator_accounting_treats_invalid_gross_price_inputs_as_unpriced() {
    let (_root, runtime) = accounting_runtime();
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-invalid-price-input".to_string(),
            activity_id: "agent_implement".to_string(),
            agent: "codex".to_string(),
            model: Some("openai/gpt-5.6-sol-2026-07-20".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 1,
                    cache_read: 2,
                    cache_create: 3,
                    cache_create_1h: 4,
                    output: 5,
                },
                provider_cost_usd: Some(0.5),
                ..InvocationTrace::default()
            },
        })
        .expect("insert invalid gross input row");

    let metrics = runtime
        .orchestrator_invocation_metrics(None, None)
        .expect("orchestrator metrics");
    let unattributed = bucket(
        &metrics.buckets,
        OrchestratorMetricsBucketKind::Unattributed,
        None,
    );
    assert_eq!(unattributed.provider_cost_count, 1);
    assert_eq!(unattributed.derived_cost_count, 0);
    assert_eq!(unattributed.unpriced_derived_count, 1);
    assert_eq!(unattributed.comparable_cost_count, 0);
}

#[test]
fn orchestrator_accounting_rejects_inverted_or_empty_effective_windows() {
    let (_root, runtime) = accounting_runtime();
    let now = Utc::now();
    assert!(
        runtime
            .orchestrator_invocation_metrics(Some(now), Some(now - Duration::seconds(1)))
            .is_err()
    );
    assert!(
        runtime
            .orchestrator_invocation_metrics(
                Some(now + Duration::minutes(1)),
                Some(now + Duration::minutes(2))
            )
            .is_err(),
        "future requests still use as_of as the effective cutoff"
    );
}
