//! Regression fixtures exercise the public scorecard API. Facts are opaque to
//! external callers; constructing them here avoids a mutable workspace fixture.
use std::collections::BTreeMap;
use std::time::Duration;

use chrono::Utc;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use super::super::facts::{Attempt, RejectOutcome, Review, TaskFacts};
use super::super::{AgentFacts, AgentQuery, AgentRole, agent_scorecard, attribution};

// (crew, tier, implemented tasks, landed tasks, reviewed tasks, first accepts).
type Cohort<'a> = (&'a str, Option<TaskComplexity>, usize, usize, usize, usize);

fn scorecard(cohorts: &[Cohort<'_>]) -> Value {
    // The served window is the rolling span ending at the query clock, so the
    // events are placed relative to that clock; fixed timestamps age out of it.
    let now = Utc::now();
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
            // One model per crew: rows group by model and recorded effort,
            // so a shared model would merge the cohorts into one row.
            attempt.runner.model = Some(format!("{crew}-model"));
            let reviews = (index < reviewed).then(|| Review {
                attempt_id: format!("{crew}-{cohort}-{index}"),
                activity_id: None,
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
        invocations: Default::default(),
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

/// `run_task` is the `(run_id, task_id)` the invocation is recorded under.
fn invocation(
    id: i64,
    (run_id, task_id): (&str, &str),
    crew: &str,
    effort: Option<orbit_types::identity::ReasoningEffort>,
    identity: bool,
    duration_ms: u64,
    at: chrono::DateTime<Utc>,
) -> orbit_store::contracts::InvocationAccountingFact {
    orbit_store::contracts::InvocationAccountingFact {
        execution: identity.then(|| orbit_types::telemetry::AgentExecutionIdentity {
            crew: Some(crew.into()),
            reasoning_effort: effort,
        }),
        id,
        job_run_id: run_id.into(),
        ts: at,
        model: Some("m1".into()),
        input_tokens: 0,
        cache_read_tokens: 0,
        cache_create_tokens: 0,
        cache_create_1h_tokens: 0,
        output_tokens: 0,
        task_ids: vec![task_id.into()],
        provider_cost_usd: None,
        derived_cost_usd: None,
        activity_id: Some("implement_one".into()),
        duration_ms,
    }
}

#[test]
fn implementer_rows_group_by_model_and_recorded_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Medium};
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    // (crew, run, invocations): three medium tasks (one also carrying a
    // shorter high invocation in the same run), two high, one provider
    // default, one with no invocation at all, and one medium task from a
    // second crew on the same model.
    type RunEffort = Option<(
        &'static str,
        bool,
        Option<orbit_types::identity::ReasoningEffort>,
    )>;
    let units: &[(&'static str, RunEffort)] = &[
        ("opus", Some(("run-m1", true, Some(Medium)))),
        ("opus", Some(("run-m2", true, Some(Medium)))),
        ("opus", Some(("run-m3", true, Some(Medium)))),
        ("opus", Some(("run-h1", true, Some(High)))),
        ("opus", Some(("run-h2", true, Some(High)))),
        ("opus", Some(("run-p1", true, None))),
        ("opus", None),
        ("other", Some(("run-o1", true, Some(Medium)))),
    ];
    let mut tasks = Vec::new();
    let mut invocations = Vec::new();
    for (index, (crew, run)) in units.iter().enumerate() {
        let task_id = format!("effort-{index}");
        let mut attempt = Attempt {
            start,
            end: Some(now),
            runner: Default::default(),
            job: true,
            tags: Vec::new(),
        };
        attempt.runner.crew = Some((*crew).into());
        attempt.runner.model = Some("m1".into());
        if let Some((run_id, identity, effort)) = run {
            attempt.runner.run_id = Some((*run_id).into());
            invocations.push(invocation(
                index as i64 * 2 + 1,
                (run_id, &task_id),
                crew,
                *effort,
                *identity,
                60_000,
                now,
            ));
            if *run_id == "run-m3" {
                // A shorter high-effort invocation in the same run does not
                // move the unit: the longest (main) invocation decides.
                invocations.push(invocation(
                    999,
                    (run_id, &task_id),
                    crew,
                    Some(High),
                    true,
                    1_000,
                    now,
                ));
            }
        }
        tasks.push(TaskFacts {
            id: task_id,
            title: "effort observation".into(),
            status: TaskStatus::Review,
            tags: Vec::new(),
            context_files: Vec::new(),
            complexity: None,
            created_at: start,
            landed: false,
            dropped: false,
            attempts: vec![attempt],
            final_attempt: Some(0),
            execution: super::super::ExecutionMode::Managed,
            landed_at: None,
            records: Vec::new(),
            reviews: Vec::new(),
            filed: None,
            warnings: Default::default(),
        });
    }
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: invocations.into(),
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
    let payload = agent_scorecard(
        &facts,
        AgentRole::Implementers,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    let by_label = |label: &str| rows.iter().find(|row| row["label"] == label).unwrap();
    assert_eq!(rows.len(), 4, "one row per (model, recorded effort)");
    let medium = by_label("m1 @ medium");
    assert_eq!(medium["units"], json!(4));
    assert_eq!(medium["reasoning_effort"], json!("medium"));
    assert_eq!(medium["crews"], json!(["opus", "other"]));
    assert_eq!(by_label("m1 @ high")["units"], json!(2));
    let provider = by_label("m1 @ provider default");
    assert_eq!(provider["units"], json!(1));
    assert!(provider["reasoning_effort"].is_null());
    let unknown = by_label("m1 @ effort unknown");
    assert_eq!(unknown["units"], json!(1));
    assert_eq!(unknown["reasoning_effort"], json!("unknown"));
}

#[test]
fn reviewer_ranks_are_dense_over_ranked_rows() {
    let now = Utc::now();
    let at = now - chrono::Duration::hours(1);
    // Three supported reviewer rows and one below-support row: the ranked
    // rows must take ranks 1, 2, 3 with no gap, and the unranked row must
    // not consume a rank (the reported 3 -> 5 skip).
    let cohorts: &[(&str, usize, ReviewVerdict)] = &[
        ("r-accept", 5, ReviewVerdict::Accept),
        ("r-mixed", 5, ReviewVerdict::AcceptWithFixes),
        ("r-incomplete", 5, ReviewVerdict::Incomplete),
        ("r-small", 1, ReviewVerdict::Accept),
    ];
    let mut tasks = Vec::new();
    for (model, count, verdict) in cohorts {
        for index in 0..*count {
            let mut review = Review {
                attempt_id: format!("{model}-{index}"),
                activity_id: None,
                at,
                verdict: *verdict,
                reviewer: Default::default(),
                findings: Vec::new(),
                reviewed: None,
                final_candidate: None,
                seconds: None,
                fix_failed: false,
                infrastructure: false,
                outcome: RejectOutcome::Pending,
            };
            review.reviewer.crew = Some("review-crew".into());
            review.reviewer.model = Some((*model).into());
            tasks.push(TaskFacts {
                id: format!("{model}-{index}"),
                title: "reviewed".into(),
                status: TaskStatus::Review,
                tags: Vec::new(),
                context_files: Vec::new(),
                complexity: None,
                created_at: at,
                landed: false,
                dropped: false,
                attempts: Vec::new(),
                final_attempt: None,
                execution: super::super::ExecutionMode::Managed,
                landed_at: None,
                records: Vec::new(),
                reviews: vec![review],
                filed: None,
                warnings: Default::default(),
            });
        }
    }
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: Default::default(),
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
    let payload = agent_scorecard(
        &facts,
        AgentRole::Reviewers,
        &AgentQuery::parse(Some("7d"), Some(5)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert!(
        rows.iter()
            .all(|row| row["label"].as_str().unwrap().ends_with("@ effort unknown")),
        "no recorded invocations means the effort-unknown row per model"
    );
    let mut ranks: Vec<i64> = rows.iter().filter_map(|row| row["rank"].as_i64()).collect();
    ranks.sort_unstable();
    assert_eq!(ranks, [1, 2, 3], "ranks are dense over the ranked rows");
    let small = rows.iter().find(|row| row["model"] == "r-small").unwrap();
    assert!(small["rank"].is_null(), "below-support rows stay unranked");
    assert_eq!(small["below_support"], json!(true));
}

/// `run_task` is the `(run_id, task_id)` the invocation is recorded under.
fn unit_invocation(
    id: i64,
    (run_id, task_id): (&str, &str),
    activity_id: &str,
    model: &str,
    effort: orbit_types::identity::ReasoningEffort,
    duration_ms: u64,
    at: chrono::DateTime<Utc>,
) -> orbit_store::contracts::InvocationAccountingFact {
    orbit_store::contracts::InvocationAccountingFact {
        execution: Some(orbit_types::telemetry::AgentExecutionIdentity {
            crew: Some("crew".into()),
            reasoning_effort: Some(effort),
        }),
        id,
        job_run_id: run_id.into(),
        ts: at,
        model: Some(model.into()),
        input_tokens: 0,
        cache_read_tokens: 0,
        cache_create_tokens: 0,
        cache_create_1h_tokens: 0,
        output_tokens: 0,
        task_ids: vec![task_id.into()],
        provider_cost_usd: None,
        derived_cost_usd: None,
        activity_id: Some(activity_id.into()),
        duration_ms,
    }
}

fn minimal_task(id: &str, at: chrono::DateTime<Utc>) -> TaskFacts {
    TaskFacts {
        id: id.into(),
        title: "multi-round effort observation".into(),
        status: TaskStatus::Done,
        tags: Vec::new(),
        context_files: Vec::new(),
        complexity: None,
        created_at: at,
        landed: true,
        dropped: false,
        attempts: Vec::new(),
        final_attempt: None,
        execution: super::super::ExecutionMode::Managed,
        landed_at: Some(at),
        records: Vec::new(),
        reviews: Vec::new(),
        filed: None,
        warnings: Default::default(),
    }
}

#[test]
fn pilot_rows_keep_each_rounds_own_effort() {
    use std::collections::BTreeSet;

    use orbit_types::identity::ReasoningEffort::{High, Medium};

    use crate::application::agents::roles::pilots::grade_all;
    use crate::application::codebase::{PilotAssessment, PilotEvidence, PilotRun};

    let now = Utc::now();
    let at = now - chrono::Duration::minutes(30);
    // One run, two rounds: the initial pilot ran high for 10s, the repair
    // pilot medium for 60s. The run-wide longest-invocation rule labelled
    // both receipts medium, pairing the initial model with an effort it
    // never ran at. A third receipt's producing worker recorded no
    // invocation in its run, so its effort stays unknown.
    let graded = grade_all(PilotEvidence {
        assessments: vec![
            PilotAssessment {
                task_id: "pilot-initial".into(),
                at,
                run: Some(PilotRun {
                    run_id: "run-pilot".into(),
                    crew: Some("crew".into()),
                    model: Some("initial-model".into()),
                    activity_id: Some("pilot".into()),
                    seconds: None,
                    tokens: None,
                }),
                after: vec!["file:src/a.rs".into()],
                author: Vec::new(),
                landed: BTreeSet::from(["src/a.rs".to_string()]),
                contested: Vec::new(),
            },
            PilotAssessment {
                task_id: "pilot-repair".into(),
                at,
                run: Some(PilotRun {
                    run_id: "run-pilot".into(),
                    crew: Some("crew".into()),
                    model: Some("repair-model".into()),
                    activity_id: Some("repair_pilot".into()),
                    seconds: None,
                    tokens: None,
                }),
                after: vec!["file:src/b.rs".into()],
                author: Vec::new(),
                landed: BTreeSet::from(["src/b.rs".to_string()]),
                contested: Vec::new(),
            },
            PilotAssessment {
                task_id: "pilot-orphan".into(),
                at,
                run: Some(PilotRun {
                    run_id: "run-orphan".into(),
                    crew: Some("crew".into()),
                    model: Some("orphan-model".into()),
                    activity_id: Some("repair_pilot".into()),
                    seconds: None,
                    tokens: None,
                }),
                after: vec!["file:src/c.rs".into()],
                author: Vec::new(),
                landed: BTreeSet::from(["src/c.rs".to_string()]),
                contested: Vec::new(),
            },
        ],
        waits: Vec::new(),
    });
    let tasks = vec![
        minimal_task("pilot-initial", at),
        minimal_task("pilot-repair", at),
        minimal_task("pilot-orphan", at),
    ];
    let invocations = vec![
        unit_invocation(
            1,
            ("run-pilot", "pilot-initial"),
            "pilot",
            "initial-model",
            High,
            10_000,
            at,
        ),
        unit_invocation(
            2,
            ("run-pilot", "pilot-repair"),
            "repair_pilot",
            "repair-model",
            Medium,
            60_000,
            at,
        ),
        // The orphan receipt's run recorded only the other round's worker.
        unit_invocation(
            3,
            ("run-orphan", "pilot-orphan"),
            "pilot",
            "orphan-model",
            High,
            60_000,
            at,
        ),
    ];
    let (charges, credits) = attribution::attribute(&tasks, &[], &graded);
    let facts = AgentFacts {
        invocations: invocations.into(),
        computed_at: now,
        tasks_scanned: tasks.len(),
        tasks,
        charges,
        credits,
        run_costs: BTreeMap::new(),
        regressions: Vec::new(),
        landed_lines: BTreeMap::new(),
        performance_types: Vec::new(),
        pilots: graded,
    };
    let payload = agent_scorecard(
        &facts,
        AgentRole::Pilots,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    let units = |label: &str| {
        rows.iter()
            .find(|row| row["label"] == label)
            .unwrap_or_else(|| panic!("row {label}: {payload}"))["units"]
            .clone()
    };
    assert_eq!(units("initial-model @ high"), json!(1));
    assert_eq!(units("repair-model @ medium"), json!(1));
    assert_eq!(units("orphan-model @ effort unknown"), json!(1));
}

#[test]
fn reviewer_rows_keep_each_settlements_own_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Medium};

    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    let first_at = start + chrono::Duration::minutes(20);
    let second_at = start + chrono::Duration::minutes(50);
    // One pipeline run, two settlements of the same reviewer model: the
    // before-PR review ran high for 10s, the before-landing review
    // medium for 60s. Neither may relabel the other.
    let reviewer = |activity_id: &str, at: chrono::DateTime<Utc>| {
        let mut review = Review {
            attempt_id: activity_id.into(),
            activity_id: Some(activity_id.into()),
            at,
            verdict: ReviewVerdict::Accept,
            reviewer: Default::default(),
            findings: Vec::new(),
            reviewed: None,
            final_candidate: None,
            seconds: None,
            fix_failed: false,
            infrastructure: false,
            outcome: RejectOutcome::Pending,
        };
        review.reviewer.crew = Some("crew".into());
        review.reviewer.model = Some("rev-model".into());
        review
    };
    let mut attempt = Attempt {
        start,
        end: Some(now),
        runner: Default::default(),
        job: true,
        tags: Vec::new(),
    };
    attempt.runner.crew = Some("impl".into());
    attempt.runner.model = Some("impl-model".into());
    attempt.runner.run_id = Some("run-pr".into());
    let mut task = minimal_task("reviewed-task", start);
    task.attempts = vec![attempt];
    task.final_attempt = Some(0);
    task.reviews = vec![
        reviewer("review", first_at),
        reviewer("landing_review", second_at),
    ];
    let tasks = vec![task];
    let invocations = vec![
        unit_invocation(
            1,
            ("run-pr", "reviewed-task"),
            "review",
            "rev-model",
            High,
            10_000,
            first_at - chrono::Duration::minutes(1),
        ),
        unit_invocation(
            2,
            ("run-pr", "reviewed-task"),
            "landing_review",
            "rev-model",
            Medium,
            60_000,
            second_at - chrono::Duration::minutes(1),
        ),
    ];
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: invocations.into(),
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
    let payload = agent_scorecard(
        &facts,
        AgentRole::Reviewers,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    let units = |label: &str| {
        rows.iter()
            .find(|row| row["label"] == label)
            .unwrap_or_else(|| panic!("row {label}: {payload}"))["units"]
            .clone()
    };
    assert_eq!(units("rev-model @ high"), json!(1));
    assert_eq!(units("rev-model @ medium"), json!(1));
}

#[test]
fn reviewer_rows_keep_re_review_settlements_own_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Low, Medium};

    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    let first_at = start + chrono::Duration::minutes(20);
    let second_at = start + chrono::Duration::minutes(35);
    let third_at = start + chrono::Duration::minutes(50);
    // One pipeline run, three before-PR settlements of the same reviewer
    // model: `review` ran high for 10s, `re_review` medium for 60s and
    // `re_review_2` low for 30s. A re-review settlement comment carries the
    // same lead as the first round's, so each is bound to the gate's `review`
    // step; none may fall to effort unknown or relabel another.
    let reviewer = |activity_id: &str, at: chrono::DateTime<Utc>| {
        let mut review = Review {
            attempt_id: activity_id.into(),
            activity_id: Some(activity_id.into()),
            at,
            verdict: ReviewVerdict::Accept,
            reviewer: Default::default(),
            findings: Vec::new(),
            reviewed: None,
            final_candidate: None,
            seconds: None,
            fix_failed: false,
            infrastructure: false,
            outcome: RejectOutcome::Pending,
        };
        review.reviewer.crew = Some("crew".into());
        review.reviewer.model = Some("rev-model".into());
        review
    };
    let mut attempt = Attempt {
        start,
        end: Some(now),
        runner: Default::default(),
        job: true,
        tags: Vec::new(),
    };
    attempt.runner.crew = Some("impl".into());
    attempt.runner.model = Some("impl-model".into());
    attempt.runner.run_id = Some("run-pr".into());
    let mut task = minimal_task("reviewed-task", start);
    task.attempts = vec![attempt];
    task.final_attempt = Some(0);
    task.reviews = vec![
        reviewer("review", first_at),
        reviewer("review", second_at),
        reviewer("review", third_at),
    ];
    let tasks = vec![task];
    let invocations = vec![
        unit_invocation(
            1,
            ("run-pr", "reviewed-task"),
            "review",
            "rev-model",
            High,
            10_000,
            first_at - chrono::Duration::minutes(1),
        ),
        unit_invocation(
            2,
            ("run-pr", "reviewed-task"),
            "re_review",
            "rev-model",
            Medium,
            60_000,
            second_at - chrono::Duration::minutes(1),
        ),
        unit_invocation(
            3,
            ("run-pr", "reviewed-task"),
            "re_review_2",
            "rev-model",
            Low,
            30_000,
            third_at - chrono::Duration::minutes(1),
        ),
    ];
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: invocations.into(),
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
    let payload = agent_scorecard(
        &facts,
        AgentRole::Reviewers,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    let units = |label: &str| {
        rows.iter()
            .find(|row| row["label"] == label)
            .unwrap_or_else(|| panic!("row {label}: {payload}"))["units"]
            .clone()
    };
    assert_eq!(units("rev-model @ high"), json!(1));
    assert_eq!(units("rev-model @ medium"), json!(1));
    assert_eq!(units("rev-model @ low"), json!(1));
    assert!(
        rows.iter()
            .all(|row| row["label"] != "rev-model @ effort unknown"),
        "{payload}"
    );
}

#[test]
fn attempt_without_a_run_cannot_borrow_a_later_runs_effort() {
    use orbit_types::identity::ReasoningEffort::High;

    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    // The same task first ran with no recorded run, then in a managed
    // run at high. The unrecorded attempt stays effort-unknown; only the
    // managed attempt counts at high.
    let make_attempt = |run_id: Option<&str>, at: chrono::DateTime<Utc>| {
        let mut attempt = Attempt {
            start: at,
            end: Some(at + chrono::Duration::minutes(5)),
            runner: Default::default(),
            job: true,
            tags: Vec::new(),
        };
        attempt.runner.crew = Some("opus".into());
        attempt.runner.model = Some("m1".into());
        attempt.runner.run_id = run_id.map(str::to_string);
        attempt
    };
    let mut task = minimal_task("borrowed-effort", start);
    task.status = TaskStatus::Review;
    task.attempts = vec![
        make_attempt(None, start),
        make_attempt(Some("run-later"), start + chrono::Duration::minutes(30)),
    ];
    task.final_attempt = Some(1);
    let tasks = vec![task];
    let invocations = vec![unit_invocation(
        1,
        ("run-later", "borrowed-effort"),
        "implement_one",
        "m1",
        High,
        60_000,
        now,
    )];
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: invocations.into(),
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
    let payload = agent_scorecard(
        &facts,
        AgentRole::Implementers,
        &AgentQuery::parse(Some("7d"), Some(1)).unwrap(),
        Duration::ZERO,
    );
    let rows = payload["rows"].as_array().unwrap();
    let mut labels: Vec<&str> = rows
        .iter()
        .map(|row| row["label"].as_str().unwrap())
        .collect();
    labels.sort_unstable();
    assert_eq!(
        labels,
        ["m1 @ effort unknown", "m1 @ high"],
        "one row per attempt population: {payload}"
    );
    assert!(rows.iter().all(|row| row["units"] == json!(1)));
}

/// One history entry as Orbit stores it, `minutes` after `base`.
fn entry(base: chrono::DateTime<Utc>, minutes: i64, fields: Value) -> Value {
    let mut fields = fields;
    fields["at"] = json!(base + chrono::Duration::minutes(minutes));
    fields
}

/// A final-recovery decision on `run`, recorded just before the transition
/// its applier writes at `minutes`.
fn decided(
    base: chrono::DateTime<Utc>,
    minutes: i64,
    run: &str,
    decision: &str,
    outcome: Value,
    fault: &str,
) -> Value {
    json!({
        "at": base + chrono::Duration::minutes(minutes) - chrono::Duration::milliseconds(10),
        "by": "system",
        "event": "final_recovery_decided",
        "note": format!("final_recovery run_id={run} decision={decision}"),
        "decision": {
            "kind": "final_recovery",
            "run_id": run,
            "decision": decision,
            "outcome": outcome,
            "fault": fault,
        },
    })
}

fn started(base: chrono::DateTime<Utc>, minutes: i64) -> Value {
    entry(
        base,
        minutes,
        json!({"by": "system", "event": "started", "from_status": "backlog", "to_status": "in_progress"}),
    )
}

fn to_review(base: chrono::DateTime<Utc>, minutes: i64) -> Value {
    entry(
        base,
        minutes,
        json!({"by": "system", "event": "status_changed", "from_status": "in_progress", "to_status": "review"}),
    )
}

fn landed(base: chrono::DateTime<Utc>, minutes: i64) -> Value {
    entry(
        base,
        minutes,
        json!({"by": "owner-landing", "event": "landing_completed", "from_status": "review", "to_status": "done"}),
    )
}

/// A rerun_ci recovery that could not read the pull request's checks and
/// escalated, recorded with `fault`.
fn rerun_ci_escalation(
    base: chrono::DateTime<Utc>,
    minutes: i64,
    run: &str,
    fault: &str,
) -> [Value; 2] {
    let reason = "rerun_ci refused: the checks of pull request #8660 cannot be read \
                  (pull request #8660 mergeability is unknown)";
    [
        decided(
            base,
            minutes,
            run,
            "rerun_ci",
            json!({"kind": "escalated", "reason": reason}),
            fault,
        ),
        entry(
            base,
            minutes,
            json!({
                "by": "system",
                "event": "final_recovery_escalated",
                "note": format!("final recovery (run_id={run}) escalated: {reason}"),
                "from_status": "review",
                "to_status": "blocked",
                "decision": {"kind": "run_transition", "run_id": run},
            }),
        ),
    ]
}

/// An operator's requeue of the unchanged, published candidate.
fn operator_requeue(base: chrono::DateTime<Utc>, minutes: i64, run: &str) -> [Value; 2] {
    [
        decided(
            base,
            minutes,
            run,
            "operator_requeue",
            json!({"kind": "requeued"}),
            "unknown",
        ),
        entry(
            base,
            minutes,
            json!({
                "by": "human:daniel",
                "event": "operator_requeued",
                "note": "requeued onto the published candidate: auto-fix found no defect; failed \
                         job was a ripgrep download timeout. Reran CI by hand: required checks \
                         green. Resume the candidate.",
                "from_status": "blocked",
                "to_status": "backlog",
            }),
        ),
    ]
}

/// A run that failed on its work, then final recovery's requeue with `fault`.
fn failed_then_requeued(
    base: chrono::DateTime<Utc>,
    minutes: i64,
    run: &str,
    fault: &str,
) -> [Value; 3] {
    [
        entry(
            base,
            minutes,
            json!({
                "by": "system",
                "event": "workflow_run_failed",
                "note": format!("workflow failed: step=validate, run_id={run}, error_code=validation_failed"),
                "from_status": "in_progress",
                "to_status": "blocked",
            }),
        ),
        decided(
            base,
            minutes + 5,
            run,
            "requeue",
            json!({"kind": "requeued"}),
            fault,
        ),
        entry(
            base,
            minutes + 5,
            json!({
                "by": "system",
                "event": "final_recovery_requeued",
                "note": format!("final recovery (run_id={run}): requeued for a fresh attempt"),
                "from_status": "blocked",
                "to_status": "backlog",
            }),
        ),
    ]
}

/// A task's facts read by the production reader and block classifier from
/// `history`, every attempt worked by `model` as a managed job.
fn observed_task(id: &str, model: &str, history: Vec<Value>) -> TaskFacts {
    use orbit_types::task::{Task, TaskHistoryEntry};
    let created = history[0]["at"].clone();
    let task: Task = serde_json::from_value(json!({
        "id": id,
        "title": format!("{id} observation"),
        "description": "fixture",
        "context_files": [format!("file:src/{id}.rs")],
        "status": "done",
        "priority": "medium",
        "task_type": "bug",
        "created_at": created,
        "updated_at": created,
    }))
    .unwrap();
    let history: Vec<TaskHistoryEntry> = history
        .into_iter()
        .map(|entry| serde_json::from_value(entry).unwrap())
        .collect();
    let classify =
        |entry: &TaskHistoryEntry| super::super::records::block_cause(entry, &history, &[]);
    let mut facts = super::super::facts::task_facts(
        &task,
        &history,
        &[],
        &[],
        &Default::default(),
        &classify,
        Utc::now(),
    );
    for attempt in &mut facts.attempts {
        attempt.job = true;
        attempt.runner.crew = Some(format!("{model}-crew"));
        attempt.runner.model = Some(model.to_string());
    }
    facts.execution = super::super::ExecutionMode::Managed;
    facts
}

#[test]
fn memory_evidence_charges_implementers_only_for_judged_implementation_faults() {
    let base = Utc::now() - chrono::Duration::hours(6);
    let created = |minutes| {
        entry(
            base,
            minutes,
            json!({"by": "claude", "event": "created", "to_status": "proposed"}),
        )
    };
    let mut infra = vec![created(0), started(base, 1), to_review(base, 15)];
    infra.extend(rerun_ci_escalation(base, 40, "jrun-a1", "unknown"));
    infra.extend(operator_requeue(base, 46, "jrun-a1"));
    infra.extend([
        entry(
            base,
            47,
            json!({"by": "hm_mac", "event": "pulled_by", "from_status": "backlog", "to_status": "in_progress", "note": "{}"}),
        ),
        entry(
            base,
            56,
            json!({"by": "hm_mac", "event": "claim_handed_off", "from_status": "in_progress", "to_status": "review"}),
        ),
        landed(base, 66),
    ]);

    // A judged implementation fault, then an infrastructure stop of the
    // retried candidate: only the first restart is the implementer's.
    let mut mixed = vec![created(0), started(base, 1)];
    mixed.extend(failed_then_requeued(base, 10, "jrun-b1", "implementer"));
    mixed.extend([started(base, 20), to_review(base, 30)]);
    mixed.extend(rerun_ci_escalation(base, 40, "jrun-b2", "infrastructure"));
    mixed.extend(operator_requeue(base, 46, "jrun-b2"));
    mixed.extend([started(base, 50), to_review(base, 60), landed(base, 70)]);

    let mut spec = vec![created(0), started(base, 1)];
    spec.extend(failed_then_requeued(base, 10, "jrun-c1", "specification"));
    spec.extend([started(base, 20), to_review(base, 30), landed(base, 40)]);

    // A block by hand, requeued by hand, with nothing judging the work.
    let mut manual = vec![created(0), started(base, 1)];
    manual.extend([
        entry(
            base,
            10,
            json!({"by": "human:daniel", "event": "status_changed", "from_status": "in_progress", "to_status": "blocked"}),
        ),
        entry(
            base,
            20,
            json!({"by": "human:daniel", "event": "status_changed", "from_status": "blocked", "to_status": "backlog"}),
        ),
        started(base, 21),
        to_review(base, 30),
        landed(base, 40),
    ]);

    // An escalation written before faults were recorded.
    let mut unjudged = vec![created(0), started(base, 1), to_review(base, 15)];
    unjudged.push(rerun_ci_escalation(base, 40, "jrun-d1", "unknown")[1].clone());
    unjudged.extend(operator_requeue(base, 46, "jrun-d1"));
    unjudged.extend([started(base, 50), to_review(base, 60), landed(base, 70)]);

    // Another model's judged fault stays in that model's pack.
    let mut other = vec![created(0), started(base, 1)];
    other.extend(failed_then_requeued(base, 10, "jrun-e1", "implementer"));
    other.extend([started(base, 20), to_review(base, 30), landed(base, 40)]);

    let tasks = vec![
        observed_task("infra", "fixture-model", infra),
        observed_task("mixed", "fixture-model", mixed),
        observed_task("spec", "fixture-model", spec),
        observed_task("manual", "fixture-model", manual),
        observed_task("unjudged", "fixture-model", unjudged),
        observed_task("other", "other-model", other),
    ];
    let (charges, credits) = attribution::attribute(&tasks, &[], &[]);
    let facts = AgentFacts {
        invocations: Default::default(),
        computed_at: Utc::now(),
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

    let claims = |model: &str| -> BTreeMap<String, Vec<(&'static str, Vec<String>)>> {
        let evidence = super::super::memory_evidence::model_evidence(&facts, model).unwrap();
        let mut claims: BTreeMap<String, Vec<(&'static str, Vec<String>)>> = BTreeMap::new();
        for problem in evidence.implementer.problems {
            for task in problem.tasks {
                claims.entry(task.task_id).or_default().push((
                    problem.description,
                    task.occurrences
                        .into_iter()
                        .map(|occurrence| occurrence.detail)
                        .collect(),
                ));
            }
        }
        claims
    };
    let implemented_wrong = "this change had to be run again, was blocked or did not merge, \
                             because of how it was implemented";

    let fixture = claims("fixture-model");
    // ORB-23194: a ripgrep download timeout, a mergeability-unknown recovery
    // refusal and a requeue of the unchanged candidate are no implementation
    // fault.
    assert!(
        !fixture.contains_key("infra"),
        "an infrastructure rerun is not an implementation problem: {fixture:?}"
    );
    assert_eq!(
        fixture.get("mixed"),
        Some(&vec![(implemented_wrong, vec!["run 2 of 3".to_string()])]),
        "the judged implementation fault stays, and only its own restart"
    );
    for task in ["spec", "manual", "unjudged", "other"] {
        assert!(
            !fixture.contains_key(task),
            "{task} must not be an implementation claim against fixture-model: {fixture:?}"
        );
    }
    assert_eq!(
        claims("other-model").get("other"),
        Some(&vec![(implemented_wrong, vec!["run 2 of 2".to_string()])]),
        "a judged fault is scoped to the model that worked the run"
    );
}
