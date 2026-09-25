use super::super::*;

use super::super::types::CURRENT_SCHEMA_VERSION;
use chrono::Utc;
use orbit_common::OrbitError;

#[test]
fn window_string_round_trips_for_all_variants() {
    // ORB-00337 AC#1 — every variant must round-trip through `as_str`/
    // `from_str`, and unknown strings yield `OrbitError::InvalidInput`.
    for w in [
        ScoreboardWindow::Hour,
        ScoreboardWindow::Day,
        ScoreboardWindow::Week,
        ScoreboardWindow::Month,
        ScoreboardWindow::All,
    ] {
        assert_eq!(w.as_str().parse::<ScoreboardWindow>().ok(), Some(w));
    }
    assert!(matches!(
        "bogus".parse::<ScoreboardWindow>(),
        Err(OrbitError::InvalidInput(_))
    ));
}

#[test]
fn orchestration_section_is_independently_versioned_and_legacy_reads_default_it() {
    let now = Utc::now();
    let temp = tempfile::tempdir().expect("create tempdir");
    let summary = generate_summary_with_inputs(
        temp.path(),
        &[],
        &ScoreboardInputs {
            orchestration: Some(OrchestrationSummary {
                schema_version: ORCHESTRATION_SCHEMA_VERSION,
                scope: "managed_execution".to_string(),
                as_of: now,
                since: Some(now - chrono::Duration::hours(1)),
                until: now,
                buckets: vec![OrchestrationBucketSummary {
                    kind: OrchestrationBucketKind::Orchestrator,
                    orchestrator: Some("sol".to_string()),
                    invocation_count: 1,
                    linked_task_count: 1,
                    input_tokens: 0,
                    cache_read_tokens: 0,
                    cache_create_tokens: 0,
                    cache_create_1h_tokens: 0,
                    output_tokens: 0,
                    provider_cost_usd: 0.0,
                    provider_cost_count: 1,
                    derived_cost_usd: 0.0,
                    derived_cost_count: 0,
                    comparable_provider_cost_usd: 0.0,
                    comparable_derived_cost_usd: 0.0,
                    comparable_cost_count: 0,
                    comparable_cost_delta_usd: 0.0,
                    missing_provider_count: 0,
                    unpriced_derived_count: 1,
                    normalized_tokens: NormalizedTokenSummary::default(),
                    models: Vec::new(),
                }],
                normalized_tokens: NormalizedTokenSummary::default(),
                previous_normalized_tokens: None,
            }),
            ..ScoreboardInputs::default()
        },
    )
    .expect("generate summary");

    let encoded = serde_json::to_value(&summary).expect("serialize summary");
    assert_eq!(encoded["schema_version"], CURRENT_SCHEMA_VERSION);
    assert!(encoded.get("duels").is_none());
    assert!(encoded.get("planning_duels").is_none());
    assert_eq!(
        encoded["orchestration"]["schema_version"],
        ORCHESTRATION_SCHEMA_VERSION
    );
    assert_eq!(
        encoded["orchestration"]["buckets"][0]["provider_cost_usd"],
        0.0
    );
    assert_eq!(
        encoded["orchestration"]["buckets"][0]["unpriced_derived_count"],
        1
    );

    let legacy: ScoreboardSummary = serde_json::from_value(serde_json::json!({
        "schema_version": 6,
        "generated_at": now.to_rfc3339(),
        "agents": {}
    }))
    .expect("deserialize v6 summary without orchestration");
    assert!(legacy.orchestration.is_none());
}
