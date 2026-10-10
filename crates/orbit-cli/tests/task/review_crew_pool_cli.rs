//! `operation.review_crew` as a pool [ORB-15195]: each after-landing batch's
//! review task draws one crew, preferring one that did not implement the
//! landed work and skipping one whose provider is at its usage limit, and
//! `orbit config show` and `orbit doctor` show the pool.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::TaskComplexity;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use serde_json::Value;

use crate::auto_task_lifecycle_cli::git;
use crate::isolated_cli_fixture::Fixture;
use crate::review_after_landing_cli::{
    commit, doctor_row, enable_review_crew, in_isolated_child, land_tasks_and_evaluate,
    open_runtime, retarget, set_policy, toggle, trigger,
};

/// The two pool members, on different providers.
const POOL: &str = r#"["sol", "grok"]"#;

/// A fresh workspace reviewing after landing through [`POOL`], with a
/// usage-limit reading on `limited_provider` when one is named; the review
/// task minted for a delivery `implementer` landed.
fn minted_review(implementer: &str, limited_provider: Option<&str>) -> (Fixture, Value) {
    let fixture = Fixture::new();
    enable_review_crew(&fixture);
    set_policy(&fixture, "crews.sol.enabled", "true");
    set_policy(&fixture, "crews.grok.enabled", "true");
    set_policy(&fixture, "operation.review_crew", POOL);
    git(&fixture, &["checkout", "-b", "fixture-delivery"]);
    commit(&fixture, "baseline\n");
    retarget(&fixture, &trigger());
    toggle(&fixture, "on");

    let runtime = open_runtime(&fixture);
    if let Some(provider) = limited_provider {
        runtime
            .record_provider_limit(&ProviderLimitObservation {
                provider: provider.into(),
                model: None,
                window: None,
                exhausted: true,
                source: ProviderLimitSource::Error,
                resets_at: Some(Utc::now() + Duration::hours(4)),
                used_percent: None,
                window_minutes: None,
                gating: true,
                partial: false,
                observed_at: Utc::now(),
                run_id: None,
                crew: None,
                detail: "Usage limit reached.".into(),
            })
            .unwrap();
    }
    let implemented = runtime
        .add_task(TaskAddParams {
            title: "Implemented work".into(),
            description: "The delivery a pooled reviewer reviews.".into(),
            acceptance_criteria: vec!["Delivered.".into()],
            plan: "1. Deliver it.".into(),
            context_files: vec!["file:fixture.txt".into()],
            complexity: TaskComplexity::Low,
            crew: Some(implementer.into()),
            ..Default::default()
        })
        .unwrap()
        .id;
    let review = land_tasks_and_evaluate(&fixture, &runtime, "landed\n", &[implemented])
        .expect("an enabled consumer mints a review task for the landed batch");
    let task = fixture.json(&["task", "show", &review, "--json"]);
    (fixture, task)
}

#[test]
fn each_batch_is_reviewed_by_a_pool_crew_that_did_not_implement_it() {
    const TEST: &str =
        "review_crew_pool_cli::each_batch_is_reviewed_by_a_pool_crew_that_did_not_implement_it";
    if !in_isolated_child(TEST) {
        return;
    }

    for (implementer, reviewer) in [("sol", "grok"), ("grok", "sol")] {
        let (_, task) = minted_review(implementer, None);
        assert_eq!(
            task["crew"], reviewer,
            "work {implementer} implemented is reviewed by {reviewer}: {task}"
        );
        assert_eq!(task["crew_source"], "operation.review_crew", "{task}");
    }

    // The independent crew's provider is at its limit, so the pool falls
    // back to the other member rather than holding the review.
    let (fixture, task) = minted_review("sol", Some("grok"));
    assert_eq!(task["crew"], "sol", "a limited crew is skipped: {task}");

    let config = fixture.json(&["config", "show", "--json"]);
    let health = &config["review"]["after_landing"]["health"];
    assert_eq!(
        health["crew_pool"],
        serde_json::json!(["grok", "sol"]),
        "{config}"
    );
    assert!(health["crew_error"].is_null(), "{config}");
    let before_pr = &config["review"]["before_pr"];
    assert_eq!(
        before_pr["crews"],
        serde_json::json!(["grok", "sol"]),
        "{config}"
    );
    assert!(before_pr["crew_selection"].is_string(), "{config}");
    let (row, _) = doctor_row(&fixture);
    assert_eq!(row["status"], "ok", "{row}");
    let message = row["message"].as_str().unwrap();
    assert!(message.contains("grok") && message.contains("sol"), "{row}");
}
