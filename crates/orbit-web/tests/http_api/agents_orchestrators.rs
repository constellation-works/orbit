//! The Orchestrators ranking (`/api/agents/orchestrators/scorecard`) through
//! the HTTP API, over orchestrators whose tasks all landed: `opus` filing
//! hard tasks, `sonnet` filing ten times as many medium tasks, and `haiku`
//! filing hard tasks with spec defects on a tenth of them.

use orbit_core::TaskStatus;
use orbit_types::task::{RegressionEscape, RegressionNature, TaskComplexity};
use serde_json::Value;

use super::agents::{Owned, WS, add, as_f64, regression};
use super::support::{Fixture, isolated, json_ok};

fn landed(
    fixture: &Fixture,
    orchestrator: &str,
    complexity: TaskComplexity,
    count: usize,
) -> Vec<String> {
    (0..count)
        .map(|_| {
            add(
                fixture,
                Owned {
                    title: "landed",
                    orchestrator: Some(orchestrator),
                    origin: None,
                    complexity,
                    status: TaskStatus::Done,
                    relations: vec![],
                },
            )
        })
        .collect()
}

fn stat<'a>(row: &'a Value, key: &str) -> &'a Value {
    &row["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stat| stat["key"] == key)
        .unwrap_or_else(|| panic!("stat {key}: {row}"))["value"]
}

/// Credit does not grow with the complexity an orchestrator's tasks were
/// assessed at, so `opus` (40 hard tasks) and `sonnet` (450 medium tasks)
/// score alike, where weighting landings 4 and 2 by complexity ranked `opus`
/// first with twice the credit; with equal raw scores the larger row ranks
/// first, because smoothing (K = 30) only pulls a row above the fleet average
/// down toward it. `haiku`, like `opus` but with spec defects on 4 of its 40
/// tasks, ranks below it. Unclassified regressions show beside the score and
/// do not change it.
#[test]
fn orchestrator_rank_follows_spec_quality_not_assessed_complexity() {
    isolated(
        "agents_orchestrators::orchestrator_rank_follows_spec_quality_not_assessed_complexity",
        || {
            let fixture = Fixture::new();
            let hard = landed(&fixture, "opus", TaskComplexity::Hard, 40);
            landed(&fixture, "sonnet", TaskComplexity::Medium, 450);
            let specs = landed(&fixture, "haiku", TaskComplexity::Hard, 40);
            for culprit in &specs[..4] {
                regression(
                    &fixture,
                    culprit,
                    RegressionNature::BrokeExisting,
                    RegressionEscape::SpecAmbiguous,
                );
            }
            regression(
                &fixture,
                &hard[0],
                RegressionNature::BrokeExisting,
                RegressionEscape::Unclassified,
            );

            let server = fixture.server(false);
            let scorecard = json_ok(server.get(&format!(
                "/api/agents/orchestrators/scorecard?{WS}&window=30d&min_support=10"
            )));
            let rows = scorecard["rows"].as_array().unwrap();
            let order: Vec<&str> = rows
                .iter()
                .map(|row| row["crew"].as_str().unwrap())
                .collect();
            assert_eq!(order, ["sonnet", "opus", "haiku"], "{scorecard}");
            let row = |crew: &str| rows.iter().find(|row| row["crew"] == crew).unwrap();
            let (hard_row, medium_row, specs_row) = (row("opus"), row("sonnet"), row("haiku"));

            assert_eq!(
                hard_row["counts"]["landed"], 40,
                "a hard landing counts once: {hard_row}"
            );
            assert_eq!(
                hard_row["raw"], medium_row["raw"],
                "the same outcomes score the same raw"
            );
            let (hard_score, medium_score) =
                (as_f64(&hard_row["score"]), as_f64(&medium_row["score"]));
            assert!(
                (medium_score - hard_score).abs() < 1.0,
                "complexity moves no score: {hard_row} {medium_row}"
            );
            assert!(
                medium_score >= hard_score,
                "40 units do not outrank 450 with the same raw score"
            );

            assert_eq!(specs_row["counts"]["spec_defects"], 4, "{specs_row}");
            assert!(
                as_f64(&hard_row["score"]) - as_f64(&specs_row["score"]) >= 5.0,
                "spec defects on a tenth of the tasks visibly cost rank: {hard_row} {specs_row}"
            );
            let fleet = as_f64(&scorecard["fleet_average"]);
            for row in rows {
                let (raw, score) = (as_f64(&row["raw"]), as_f64(&row["score"]));
                assert!(score <= raw, "smoothing never raises a score: {row}");
                if raw > fleet {
                    assert!(
                        fleet < score && score < raw,
                        "pulled down toward the average: {row}"
                    );
                } else {
                    assert_eq!(
                        score, raw,
                        "a row below the average keeps its raw score: {row}"
                    );
                }
            }

            assert_eq!(stat(hard_row, "unclassified"), 1, "{hard_row}");
            assert_eq!(stat(medium_row, "unclassified"), 0, "{medium_row}");
            assert_eq!(
                (
                    hard_row["counts"]["spec_defects"].as_f64(),
                    hard_row["counts"]["spec_rework"].as_f64()
                ),
                (Some(0.0), Some(0.0)),
                "an unclassified regression is not scored: {hard_row}"
            );
        },
    );
}
