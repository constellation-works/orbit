//! Regression fixtures exercise the public scorecard API. Facts are opaque to
//! external callers; constructing them here avoids a mutable workspace fixture.
use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use super::super::facts::{Attempt, RejectOutcome, Review, TaskFacts};
use super::super::{AgentFacts, AgentQuery, AgentRole, agent_scorecard, attribution};

// (crew, tier, implemented tasks, landed tasks, reviewed tasks, first accepts).
type Cohort<'a> = (&'a str, Option<TaskComplexity>, usize, usize, usize, usize);

fn scorecard(cohorts: &[Cohort<'_>]) -> Value {
    let now = Utc.with_ymd_and_hms(2026, 11, 8, 0, 0, 0).unwrap();
    let start = now - chrono::Duration::hours(1);
    let mut tasks = Vec::new();
    for (cohort, &(crew, complexity, units, landed, reviewed, accepted)) in
        cohorts.iter().enumerate()
    {
        for index in 0..units {
            let mut attempt = Attempt {
                start,
                end: Some(now),
                runner: Default::default(),
                job: true,
                tags: Vec::new(),
            };
            attempt.runner.crew = Some(crew.into());
            attempt.runner.model = Some("fixture".into());
            let reviews = (index < reviewed).then(|| Review {
                attempt_id: format!("{crew}-{cohort}-{index}"),
                at: start + chrono::Duration::minutes(1),
                verdict: if index < accepted {
                    ReviewVerdict::Accept
                } else if index % 2 == 0 {
                    ReviewVerdict::Reject
                } else {
                    ReviewVerdict::AcceptWithFixes
                },
                reviewer: Default::default(),
                findings: Vec::new(),
                reviewed: None,
                final_candidate: None,
                seconds: None,
                fix_failed: false,
                infrastructure: false,
                outcome: RejectOutcome::Pending,
            });
            tasks.push(TaskFacts {
                id: format!("{crew}-{cohort}-{index}"),
                title: "scorecard observation".into(),
                status: if index < landed {
                    TaskStatus::Done
                } else {
                    TaskStatus::Review
                },
                orchestrator: None,
                orchestrator_model: None,
                origin: None,
                tags: Vec::new(),
                context_files: Vec::new(),
                complexity,
                created_at: start,
                landed: index < landed,
                dropped: false,
                attempts: vec![attempt],
                final_attempt: Some(0),
                execution: super::super::ExecutionMode::Managed,
                landed_at: (index < landed).then_some(now),
                records: Vec::new(),
                reviews: reviews.into_iter().collect(),
                filed: None,
                warnings: Default::default(),
            });
        }
    }
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: Vec::new(),
        computed_at: now,
        tasks_scanned: tasks.len(),
        tasks,
        charges,
        credits,
        run_costs: BTreeMap::new(),
        regressions: Vec::new(),
        landed_lines: BTreeMap::new(),
        performance_types: Vec::new(),
        pilots: Vec::new(),
    };
    agent_scorecard(
        &facts,
        AgentRole::Implementers,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    )
}

fn row<'a>(payload: &'a Value, crew: &str) -> &'a Value {
    payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["crew"] == crew)
        .unwrap()
}

fn adjustments(row: &Value) -> &Value {
    &row["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stat| stat["key"] == "tier_adjustments")
        .unwrap()["value"]
}

fn assert_conserved(payload: &Value) {
    for term in payload["terms"].as_array().unwrap() {
        let key = term["key"].as_str().unwrap();
        // Escapes have a separate leave-one-row-out size reference.
        if key == "escaped_regressions" {
            continue;
        }
        let sum = |field: &str| {
            payload["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[field][key].as_f64().unwrap())
                .sum::<f64>()
        };
        assert!(
            (sum("expected") - sum("counts")).abs() < 0.03,
            "{key}: fleet expected counts must be conserved (JSON rounds each row)"
        );
        for row in payload["rows"].as_array().unwrap() {
            let units = row
                .get("term_units")
                .and_then(|units| units.get(key))
                .unwrap_or(&row["units"])
                .as_f64()
                .unwrap();
            assert!(row["expected"][key].as_f64().unwrap() >= 0.0);
            if key != "crew_rework" {
                assert!(row["expected"][key].as_f64().unwrap() <= units + 0.01);
            }
        }
    }
}

#[test]
fn sparse_tier_scorecard_retains_own_evidence_and_conserves_fleet() {
    use TaskComplexity::{Hard, Low, Medium, XHard};
    let payload = scorecard(&[
        ("common", Some(Medium), 100, 100, 60, 60),
        ("adjacent", Some(Hard), 30, 15, 10, 5),
        ("rare", Some(XHard), 10, 0, 10, 0),
    ]);
    let rare = row(&payload, "rare");
    // The old implementation gave this crew the medium pool's 9.09 expected
    // landings. Borrowing from adjacent hard retains the rare tier's failures.
    assert_eq!(rare["expected"]["weighted_landed"], json!(3.75));
    assert_eq!(rare["expected"]["first_pass_accepts"], json!(3.75));
    assert_eq!(
        row(&payload, "common")["expected"]["weighted_landed"],
        json!(100)
    );
    assert_eq!(adjustments(rare)[0]["tier"], "xhard");
    assert_eq!(adjustments(rare)[0]["adjusted"], true);
    assert_eq!(adjustments(rare)[0]["reference"], "hard");
    assert_eq!(adjustments(rare)[0]["reference_tiers"], json!(["hard"]));
    assert_eq!(
        adjustments(rare)[0]["terms"]["weighted_landed"]["own_weight"],
        json!(0.25)
    );
    assert_conserved(&payload);
    // Changing only the rare tier's own outcomes changes its expectation;
    // a wholesale replacement with the adjacent tier's rate cannot do this.
    let changed = scorecard(&[
        ("common", Some(Medium), 100, 100, 60, 60),
        ("adjacent", Some(Hard), 30, 15, 10, 5),
        ("rare", Some(XHard), 10, 10, 10, 10),
    ]);
    assert_eq!(
        row(&changed, "rare")["expected"]["weighted_landed"],
        json!(6.25)
    );
    assert_eq!(
        row(&changed, "rare")["expected"]["first_pass_accepts"],
        json!(6.25)
    );
    assert_conserved(&changed);

    // Ties prefer the lower supported tier; pooling remains defined with no
    // supported tier, an unassessed tier, or a reference with no reviews.
    for (cohorts, crew, reference) in [
        (
            vec![
                ("low", Some(Low), 30, 30, 0, 0),
                ("hard", Some(Hard), 100, 20, 70, 20),
                ("rare", Some(Medium), 10, 0, 10, 0),
            ],
            "rare",
            "low",
        ),
        (
            vec![
                ("one", Some(Low), 10, 10, 5, 5),
                ("two", Some(XHard), 10, 0, 10, 0),
            ],
            "two",
            "fleet_pool",
        ),
        (
            vec![
                ("one", Some(Hard), 30, 30, 10, 10),
                ("unknown", None, 10, 0, 10, 0),
            ],
            "unknown",
            "supported_pool",
        ),
        // Several sparse tiers can exhaust the reference's reviewed exposure:
        // simultaneous exchanges must scale down without negative expectations.
        (
            vec![
                ("ref", Some(Medium), 30, 30, 1, 1),
                ("low", Some(Low), 29, 0, 29, 0),
                ("hard", Some(Hard), 29, 0, 29, 0),
                ("rare", Some(XHard), 29, 0, 29, 0),
            ],
            "rare",
            "medium",
        ),
    ] {
        let payload = scorecard(&cohorts);
        assert_eq!(adjustments(row(&payload, crew))[0]["reference"], reference);
        assert_conserved(&payload);
    }
}

#[test]
fn supported_tier_scorecard_keeps_existing_scores() {
    use TaskComplexity::{Hard, Low, Medium, XHard};
    let mut cohorts = Vec::new();
    for tier in [Some(Low), Some(Medium), Some(Hard), Some(XHard), None] {
        // Two rows share every tier: the all-supported reference is exactly
        // 0.5 landed and 1.0 first accepts, giving raw 150/50 and shrunk 135.71/50.
        cohorts.push(("a", tier, 15, 15, 15, 15));
        cohorts.push(("b", tier, 15, 0, 15, 15));
    }
    let payload = scorecard(&cohorts);
    assert_eq!(row(&payload, "a")["raw"], json!(150));
    assert_eq!(row(&payload, "a")["score"], json!(135.71));
    assert_eq!(row(&payload, "b")["raw"], json!(50));
    assert_eq!(row(&payload, "b")["score"], json!(50));
    assert_conserved(&payload);
}
