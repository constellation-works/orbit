//! The model ranking over fixture facts at its public API: pooling, the
//! settled window, eligibility, shrinkage, bands, within-band order, unmeasured
//! measures, parity with the implementer scorecard's expectations and
//! calibrated seasons, and the top band reserved for fully measured models.
use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use orbit_types::task::{
    DefectSeverity, RegressionClassification, RegressionEscape, RegressionNature, SeveritySource,
    TaskComplexity, TaskStatus,
};
use orbit_types::workflow::ReviewVerdict;
use serde_json::Value;

use super::super::facts::{Attempt, RegressionLink, RejectOutcome, Review, TaskFacts};
use super::super::{
    AgentFacts, AgentQuery, AgentRole, ExecutionMode, RankQuery, RankSeasons, agent_rank,
    agent_rank_calibration, agent_scorecard, attribution,
};

/// One model's fixture work: crews it runs under, tasks per tier (low and
/// medium alternate), each task's first verdict by index, how many of its
/// tasks regress, its cost per task, how many days ago it started, and
/// whether its tasks carry a settled review (`false` leaves them unreviewed).
struct Work<'a> {
    model: Option<&'a str>,
    crews: &'a [&'a str],
    tasks: usize,
    verdict: fn(usize) -> ReviewVerdict,
    regressed: usize,
    cost: f64,
    days_ago: i64,
    reviewed: bool,
}

fn facts(work: &[Work<'_>]) -> AgentFacts {
    let now = Utc::now();
    let mut tasks = Vec::new();
    let mut regressions = Vec::new();
    let mut run_costs = BTreeMap::new();
    for (group, work) in work.iter().enumerate() {
        let start: DateTime<Utc> = now - chrono::Duration::days(work.days_ago);
        for index in 0..work.tasks {
            let id = format!("T{group}-{index}");
            let run_id = format!("run-{id}");
            run_costs.insert(run_id.clone(), Some(work.cost));
            let mut attempt = Attempt {
                start,
                end: Some(start + chrono::Duration::hours(1)),
                runner: Default::default(),
                job: true,
                local: true,
                implementer: true,
                tags: Vec::new(),
            };
            attempt.runner.crew = Some(work.crews[index % work.crews.len()].into());
            attempt.runner.model = work.model.map(Into::into);
            attempt.runner.run_id = Some(run_id);
            let review = Review {
                attempt_id: id.clone(),
                activity_id: None,
                at: start + chrono::Duration::minutes(30),
                verdict: (work.verdict)(index),
                reviewer: Default::default(),
                findings: Vec::new(),
                reviewed: None,
                final_candidate: None,
                seconds: None,
                fix_failed: false,
                infrastructure: false,
                outcome: RejectOutcome::Pending,
            };
            if index < work.regressed {
                regressions.push(RegressionLink {
                    culprit: id.clone(),
                    regressing: format!("R-{id}"),
                    at: start + chrono::Duration::days(2),
                    classification: RegressionClassification {
                        nature: RegressionNature::BrokeExisting,
                        escape: RegressionEscape::NoTest,
                        severity: DefectSeverity::Medium,
                        severity_source: SeveritySource::Filer,
                        sites: Vec::new(),
                    },
                });
            }
            tasks.push(TaskFacts {
                id,
                title: "ranking observation".into(),
                status: TaskStatus::Done,
                tags: Vec::new(),
                context_files: Vec::new(),
                complexity: Some(if index % 2 == 0 {
                    TaskComplexity::Low
                } else {
                    TaskComplexity::Medium
                }),
                created_at: start,
                landed: true,
                dropped: false,
                attempts: vec![attempt],
                final_attempt: Some(0),
                execution: ExecutionMode::Managed,
                landed_at: Some(start + chrono::Duration::hours(2)),
                records: Vec::new(),
                reviews: if work.reviewed {
                    vec![review]
                } else {
                    Vec::new()
                },
                filed: None,
                warnings: Default::default(),
            });
        }
    }
    let (charges, credits) = attribution::attribute(&tasks, &regressions, &[]);
    AgentFacts {
        invocations: Default::default(),
        launches: Default::default(),
        computed_at: now,
        tasks_scanned: tasks.len(),
        tasks,
        charges,
        credits,
        run_costs,
        regressions,
        landed_lines: BTreeMap::new(),
        performance_types: Vec::new(),
        pilots: Vec::new(),
    }
}

fn accept(_: usize) -> ReviewVerdict {
    ReviewVerdict::Accept
}

fn mixed(index: usize) -> ReviewVerdict {
    match index % 4 {
        0 | 1 => ReviewVerdict::Accept,
        2 => ReviewVerdict::Reject,
        _ => ReviewVerdict::AcceptWithFixes,
    }
}

fn one_better(index: usize) -> ReviewVerdict {
    if index == 2 {
        ReviewVerdict::Accept
    } else {
        mixed(index)
    }
}

fn poor(index: usize) -> ReviewVerdict {
    if index.is_multiple_of(5) {
        ReviewVerdict::Accept
    } else {
        ReviewVerdict::Reject
    }
}

fn fleet() -> AgentFacts {
    let settled = 20;
    facts(&[
        // Two crews of one model pool into one row.
        Work {
            model: Some("strong"),
            crews: &["strong-a", "strong-b"],
            tasks: 40,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        Work {
            model: Some("middle"),
            crews: &["middle"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 2.0,
            days_ago: settled,
            reviewed: true,
        },
        // Same score as middle but cheaper, so it wins an exact-score tie.
        Work {
            model: Some("twin"),
            crews: &["twin"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        // A slightly higher score but a higher cost stays first in the band.
        Work {
            model: Some("near"),
            crews: &["near"],
            tasks: 40,
            verdict: one_better,
            regressed: 4,
            cost: 4.0,
            days_ago: settled,
            reviewed: true,
        },
        // Same score and cost as twin: the final tie-break is model name.
        Work {
            model: Some("alpha"),
            crews: &["alpha"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        Work {
            model: Some("weak"),
            crews: &["weak"],
            tasks: 40,
            verdict: poor,
            regressed: 12,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        Work {
            model: Some("small"),
            crews: &["small"],
            tasks: 10,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        Work {
            model: None,
            crews: &["anonymous"],
            tasks: 5,
            verdict: mixed,
            regressed: 0,
            cost: 1.0,
            days_ago: settled,
            reviewed: true,
        },
        // Inside the settle lag: only the live board sees it.
        Work {
            model: Some("recent"),
            crews: &["recent"],
            tasks: 3,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: 3,
            reviewed: true,
        },
    ])
}

fn settled() -> RankQuery {
    RankQuery::parse(None, None, None, None, false).unwrap()
}

fn row<'a>(payload: &'a Value, model: &str) -> &'a Value {
    payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["model"] == model)
        .unwrap_or_else(|| panic!("no {model} row"))
}

fn f(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("not a number: {value}"))
}

#[test]
fn rank_pools_models_bands_by_bootstrap_orders_by_cost_and_drops_unmeasured_weights() {
    let facts = fleet();
    let payload = agent_rank(&facts, &settled(), &RankSeasons::default(), Duration::ZERO);

    // Rows are models, crews pooled; unknown models are only unattributed.
    let models: Vec<&str> = payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["model"].as_str().unwrap())
        .collect();
    assert_eq!(
        models,
        ["strong", "near", "alpha", "twin", "middle", "weak", "small"],
        "board order"
    );
    assert_eq!(row(&payload, "strong")["units"], 40);
    assert_eq!(
        row(&payload, "strong")["crews"],
        serde_json::json!([
            {"crew": "strong-a", "tasks": 20},
            {"crew": "strong-b", "tasks": 20},
        ])
    );
    assert_eq!(payload["unattributed"]["tasks"], 5);
    assert_eq!(payload["live"], false);
    assert_eq!(payload["scales_source"], "window_estimate");
    assert!(payload["season"].is_null());

    // Six eligible models qualify the board. Near scores above the exact
    // score tie but shares its band despite higher cost. The exact tie is
    // ordered by cost, then by model name.
    assert_eq!(payload["qualified"], true);
    assert_eq!(payload["eligible_models"], 6);
    let labels: Vec<&str> = payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["rank_label"].as_str().unwrap_or("-"))
        .collect();
    // Band ranks are dense: the shared 2= band is followed by 3, never 4,
    // and the unranked ineligible row consumes no rank.
    assert_eq!(labels, ["1", "2=", "2=", "2=", "2=", "3", "-"]);
    assert!(f(&row(&payload, "near")["score"]) > f(&row(&payload, "alpha")["score"]));
    assert_eq!(
        f(&row(&payload, "alpha")["score"]),
        f(&row(&payload, "twin")["score"])
    );
    assert_eq!(
        f(&row(&payload, "twin")["score"]),
        f(&row(&payload, "middle")["score"])
    );
    assert_eq!(
        f(&row(&payload, "alpha")["cost"]["ratio"]),
        f(&row(&payload, "twin")["cost"]["ratio"])
    );
    assert!(
        f(&row(&payload, "near")["cost"]["ratio"]) > f(&row(&payload, "middle")["cost"]["ratio"])
    );
    assert_eq!(row(&payload, "middle")["cost"]["cost_unknown"], false);
    assert!(f(&payload["paired_probabilities"]["strong"]["near"]) >= 0.8);
    assert!(f(&payload["paired_probabilities"]["near"]["alpha"]) < 0.95);
    assert!(f(&payload["paired_probabilities"]["alpha"]["twin"]) < 0.95);
    let small = row(&payload, "small");
    assert_eq!(small["eligible"], false);
    assert!(small["rank"].is_null() && small["interval"].is_null());
    assert!(
        small["ineligible_reason"]
            .as_str()
            .unwrap()
            .contains("30 needed")
    );
    assert!(
        small["score"].is_number(),
        "an ineligible model still shows a score"
    );

    for model in ["strong", "near", "alpha", "twin", "middle", "weak"] {
        let row = row(&payload, model);
        // Measures without recorded events, and measures that never vary
        // (no rework in the fixture), leave the weight denominator.
        for key in ["ci_failures", "final_recoveries", "tool_call_errors"] {
            assert_eq!(
                row["measures"][key]["status"], "unmeasured",
                "{model} {key}"
            );
            assert!(row["measures"][key]["contribution"].is_null());
        }
        assert_eq!(row["measures"]["crew_rework"]["status"], "no_scale");
        assert_eq!(
            f(&row["weights_measured"]),
            50.0,
            "{model}: escapes 30 + review 20"
        );
        // Contributions add up to the raw score; shrinkage toward the
        // others' units-weighted mean gives the score.
        let contributions: f64 = ["escaped_regressions", "review_outcome"]
            .iter()
            .map(|key| f(&row["measures"][*key]["contribution"]))
            .sum();
        assert!(
            (100.0 + contributions - f(&row["raw_score"])).abs() < 0.02,
            "{model}"
        );
        let (z, z_loo, units) = (f(&row["z"]), f(&row["z_loo"]), f(&row["units"]));
        let shrunk = (units * z + 30.0 * z_loo) / (units + 30.0);
        assert!(
            (100.0 + 10.0 * shrunk - f(&row["score"])).abs() < 0.01,
            "{model}"
        );
        let interval = &row["interval"];
        assert!(f(&interval["p10"]) <= f(&interval["p90"]));
        let review = &row["measures"]["review_outcome"];
        assert_eq!(review["units"], 40);
        let residual = (f(&review["actual"]) - f(&review["expected"])) / 40.0;
        assert!((residual - f(&review["residual"])).abs() < 0.01, "{model}");
    }
    assert!(f(&row(&payload, "strong")["score"]) > f(&row(&payload, "weak")["score"]));
    assert!(
        payload["measures"]
            .as_array()
            .unwrap()
            .iter()
            .any(|measure| {
                measure["key"] == "ci_failures" && measure["status"] == "unmeasured"
            })
    );

    // The settle lag keeps the newest work out; live includes it.
    assert!(
        payload["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["model"] != "recent")
    );
    let live = RankQuery::parse(None, None, None, None, true).unwrap();
    let live = agent_rank(&facts, &live, &RankSeasons::default(), Duration::ZERO);
    assert_eq!(live["provisional"], true);
    assert_eq!(row(&live, "recent")["units"], 3);

    rank_admits_single_tier_models_and_flags_them();
}

fn rank_admits_single_tier_models_and_flags_them() {
    let days = 20;
    let mut fleet_facts = facts(&[
        Work {
            model: Some("solo"),
            crews: &["solo"],
            tasks: 40,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: days,
            reviewed: true,
        },
        Work {
            model: Some("strong"),
            crews: &["strong-a"],
            tasks: 40,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: days,
            reviewed: true,
        },
        Work {
            model: Some("middle"),
            crews: &["middle"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 2.0,
            days_ago: days,
            reviewed: true,
        },
        Work {
            model: Some("weak"),
            crews: &["weak"],
            tasks: 40,
            verdict: poor,
            regressed: 12,
            cost: 1.0,
            days_ago: days,
            reviewed: true,
        },
        // Enough tasks overall but no assessed tier: stays ineligible.
        Work {
            model: Some("unassessed-only"),
            crews: &["unassessed-only"],
            tasks: 35,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: days,
            reviewed: true,
        },
    ]);
    // Solo works only the hard tier; unassessed-only has no assessed tier.
    for task in &mut fleet_facts.tasks {
        if task.id.starts_with("T0-") {
            task.complexity = Some(TaskComplexity::Hard);
        }
        if task.id.starts_with("T4-") {
            task.complexity = None;
        }
    }
    let payload = agent_rank(
        &fleet_facts,
        &settled(),
        &RankSeasons::default(),
        Duration::ZERO,
    );
    assert_eq!(payload["rules"]["min_tiers"], 1);
    let solo = row(&payload, "solo");
    assert_eq!(
        solo["eligible"], true,
        "single-tier model is ranked: {solo}"
    );
    assert_eq!(solo["single_tier"], "hard");
    assert!(solo["rank"].is_number() && solo["interval"].is_object());
    assert!(
        payload["paired_probabilities"]["solo"]["strong"].is_number(),
        "paired probabilities include the single-tier model"
    );
    assert_eq!(row(&payload, "strong")["single_tier"], Value::Null);
    let unassessed = row(&payload, "unassessed-only");
    assert_eq!(unassessed["eligible"], false);
    assert_eq!(unassessed["single_tier"], Value::Null);
    assert!(
        unassessed["ineligible_reason"]
            .as_str()
            .unwrap()
            .contains("1 needed"),
        "{}",
        unassessed["ineligible_reason"]
    );

    // A homogeneous fleet has no nonzero noise scales, even with enough
    // tasks in an assessed tier. It must not be labeled single-tier.
    let mut homogeneous = facts(&[Work {
        model: Some("no-scale"),
        crews: &["no-scale"],
        tasks: 40,
        verdict: accept,
        regressed: 0,
        cost: 1.0,
        days_ago: days,
        reviewed: true,
    }]);
    for task in &mut homogeneous.tasks {
        task.complexity = Some(TaskComplexity::Hard);
    }
    let payload = agent_rank(
        &homogeneous,
        &settled(),
        &RankSeasons::default(),
        Duration::ZERO,
    );
    let no_scale = row(&payload, "no-scale");
    assert_eq!(no_scale["units"], 40);
    assert_eq!(no_scale["eligible"], false);
    assert!(no_scale["score"].is_null());
    assert_eq!(
        no_scale["ineligible_reason"],
        "no measure with units and a scale"
    );
    assert!(no_scale["single_tier"].is_null());
}

#[test]
fn rank_credits_each_crew_only_its_own_pooled_tasks() {
    // Two disjoint tasks by different crews at the same model and effort
    // pool into one model row: each crew is credited only its own task.
    let facts = facts(&[Work {
        model: Some("pooled"),
        crews: &["crew-a", "crew-b"],
        tasks: 2,
        verdict: accept,
        regressed: 0,
        cost: 1.0,
        days_ago: 20,
        reviewed: true,
    }]);
    let payload = agent_rank(&facts, &settled(), &RankSeasons::default(), Duration::ZERO);
    let pooled = row(&payload, "pooled");
    assert_eq!(pooled["units"], 2);
    assert_eq!(
        pooled["crews"],
        serde_json::json!([
            {"crew": "crew-a", "tasks": 1},
            {"crew": "crew-b", "tasks": 1},
        ])
    );
}

#[test]
fn rank_expectations_match_the_implementer_scorecard_on_shared_terms() {
    let facts = fleet();
    let payload = agent_rank(&facts, &settled(), &RankSeasons::default(), Duration::ZERO);
    let until = payload["until"].as_str().unwrap();
    let scorecard = agent_scorecard(
        &facts,
        AgentRole::Implementers,
        &AgentQuery::parse(Some("30d"), Some(1))
            .unwrap()
            .with_until(Some(until))
            .unwrap(),
        Duration::ZERO,
    );
    for model in ["strong", "middle", "twin", "weak", "small"] {
        // A model's expectation is the sum of its crews' scorecard rows.
        let crews: Vec<&Value> = scorecard["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["model"] == model)
            .collect();
        assert!(!crews.is_empty(), "{model} has scorecard rows");
        let sum =
            |field: &str, term: &str| crews.iter().map(|row| f(&row[field][term])).sum::<f64>();
        let measures = &row(&payload, model)["measures"];
        let close = |a: f64, b: f64| (a - b).abs() <= 0.02 * crews.len() as f64;
        for term in ["escaped_regressions", "crew_rework"] {
            assert!(
                close(f(&measures[term]["expected"]), sum("expected", term)),
                "{model} {term}: {} vs {}",
                measures[term]["expected"],
                sum("expected", term)
            );
            assert!(
                close(f(&measures[term]["actual"]), sum("counts", term)),
                "{model} {term}"
            );
        }
        let review = |field: &str| {
            sum(field, "first_pass_accepts")
                - sum(field, "review_rejects")
                - 0.5 * sum(field, "reviewer_repairs")
        };
        assert!(
            close(
                f(&measures["review_outcome"]["expected"]),
                review("expected")
            ),
            "{model} review outcome expected"
        );
        assert!(
            close(f(&measures["review_outcome"]["actual"]), review("counts")),
            "{model} review outcome"
        );
    }
}

#[test]
fn calibrated_season_stores_window_and_reference_size_and_drives_the_board() {
    let facts = fleet();
    let query = settled();
    let seasons = agent_rank_calibration(&facts, &query, &RankSeasons::default()).unwrap();
    let payload = agent_rank(&facts, &query, &seasons, Duration::ZERO);
    assert_eq!(payload["scales_source"], "season");
    let season = &payload["season"];
    assert_eq!(season["season"], 0);
    assert_eq!(
        season["since"].as_str().unwrap()[..10],
        payload["since"].as_str().unwrap()[..10]
    );
    assert_eq!(
        season["until"].as_str().unwrap()[..10],
        payload["until"].as_str().unwrap()[..10]
    );
    assert_eq!(season["n_ref"], 40, "median eligible model's tasks");
    assert_eq!(season["draws"], 1000);
    let scales = season["scales"].as_array().unwrap();
    let scale = |key: &str| &scales.iter().find(|scale| scale["key"] == key).unwrap()["scale"];
    assert!(
        scale("ci_failures").is_null(),
        "an unrecorded measure has no scale"
    );
    assert!(f(scale("review_outcome")) > 0.0);
    let measure = payload["measures"]
        .as_array()
        .unwrap()
        .iter()
        .find(|measure| measure["key"] == "review_outcome")
        .unwrap();
    assert_eq!(measure["scale"], *scale("review_outcome"));

    // Seasons append; a live window is never calibrated.
    let next = agent_rank_calibration(&facts, &query, &seasons).unwrap();
    let payload = agent_rank(&facts, &query, &next, Duration::ZERO);
    assert_eq!(payload["season"]["season"], 1);
    let live = RankQuery::parse(None, None, None, None, true).unwrap();
    assert!(agent_rank_calibration(&facts, &live, &seasons).is_err());
}

/// A model whose tasks predate review recording has no reviewed deliveries,
/// so review outcome leaves its weight denominator and its score rests on
/// escaped regressions alone. Its zero escapes sit in an era with few recorded
/// regressions, which reads as a large favourable residual. Such a row must
/// not hold the top band over a model with the board's full measure coverage.
#[test]
fn rank_keeps_partly_measured_model_out_of_top_band() {
    let facts = facts(&[
        Work {
            model: Some("early"),
            crews: &["early"],
            tasks: 40,
            verdict: accept,
            regressed: 0,
            cost: 1.0,
            days_ago: 20,
            reviewed: false,
        },
        Work {
            model: Some("full"),
            crews: &["full"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 1.0,
            days_ago: 20,
            reviewed: true,
        },
        Work {
            model: Some("peer"),
            crews: &["peer"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 2.0,
            days_ago: 20,
            reviewed: true,
        },
        Work {
            model: Some("other"),
            crews: &["other"],
            tasks: 40,
            verdict: mixed,
            regressed: 4,
            cost: 3.0,
            days_ago: 20,
            reviewed: true,
        },
    ]);
    let payload = agent_rank(&facts, &settled(), &RankSeasons::default(), Duration::ZERO);
    assert_eq!(payload["qualified"], true);

    let early = row(&payload, "early");
    assert_eq!(early["eligible"], true, "early is ranked, only not on top");
    assert_ne!(early["rank"], 1, "a partly measured model holds band 1");
    assert!(
        early["top_band_ineligible_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("without Review outcome")),
        "the row states why it is kept out of the top band: {early}"
    );

    let full_weight = f(&row(&payload, "full")["weights_measured"]);
    let top: Vec<&Value> = payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["rank"] == 1)
        .collect();
    assert!(!top.is_empty(), "the board has a top band");
    for row in top {
        assert_eq!(
            f(&row["weights_measured"]),
            full_weight,
            "{} ranks first on less measured weight",
            row["model"]
        );
        assert!(row["top_band_ineligible_reason"].is_null());
    }
}
