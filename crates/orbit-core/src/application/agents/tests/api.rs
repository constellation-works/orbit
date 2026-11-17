//! Regression fixtures exercise the public scorecard API. Facts are opaque to
//! external callers; constructing them here avoids a mutable workspace fixture.
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use orbit_store::contracts::{AgentExecutionRecord, InvocationAccountingFact};
use orbit_types::identity::ReasoningEffort;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::telemetry::AgentExecutionIdentity;
use orbit_types::workflow::ReviewVerdict;
use serde_json::{Value, json};

use super::super::facts::{Attempt, Finding, FindingState, RejectOutcome, Review, TaskFacts};
use super::super::roles::pilots::{Graded, grade_all};
use super::super::{
    AgentFacts, AgentQuery, AgentRole, ExecutionMode, agent_scorecard, attribution,
};
use crate::application::codebase::{PilotAssessment, PilotEvidence, PilotRun};

// (crew, tier, implemented tasks, landed tasks, reviewed tasks, first accepts).
type Cohort<'a> = (&'a str, Option<TaskComplexity>, usize, usize, usize, usize);

fn scorecard(cohorts: &[Cohort<'_>]) -> Value {
    // Fixed timestamps age out of the served rolling window; use its clock.
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    let mut tasks = Vec::new();
    for (cohort, &(crew, complexity, units, landed, reviewed, accepted)) in
        cohorts.iter().enumerate()
    {
        for index in 0..units {
            let id = format!("{crew}-{cohort}-{index}");
            // A shared model would merge cohorts: rows group by model and effort.
            let attempt = implementer_attempt(start, now, crew, &format!("{crew}-model"), None);
            let verdict = if index < accepted {
                ReviewVerdict::Accept
            } else if index % 2 == 0 {
                ReviewVerdict::Reject
            } else {
                ReviewVerdict::AcceptWithFixes
            };
            let landed_here = index < landed;
            let reviews =
                (index < reviewed).then(|| review_of(&id, None, plus_minutes(start, 1), verdict));
            tasks.push(TaskFacts {
                complexity,
                status: if landed_here {
                    TaskStatus::Done
                } else {
                    TaskStatus::Review
                },
                landed: landed_here,
                landed_at: landed_here.then_some(now),
                attempts: vec![attempt],
                final_attempt: Some(0),
                reviews: reviews.into_iter().collect(),
                ..minimal_task(&id, start)
            });
        }
    }
    role_scorecard(&scan_tasks(now, tasks), AgentRole::Implementers)
}

fn matching_row<'a>(payload: &'a Value, field: &str, value: &str) -> &'a Value {
    payload["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row[field] == value)
        .unwrap_or_else(|| panic!("no {field}={value} row: {payload}"))
}

fn row<'a>(payload: &'a Value, crew: &str) -> &'a Value {
    matching_row(payload, "crew", crew)
}

fn labelled_row<'a>(payload: &'a Value, label: &str) -> &'a Value {
    matching_row(payload, "label", label)
}

fn model_row<'a>(payload: &'a Value, model: &str) -> &'a Value {
    matching_row(payload, "model", model)
}

fn units_of(payload: &Value, label: &str) -> Value {
    labelled_row(payload, label)["units"].clone()
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

/// The `AgentFacts` a scan of `tasks` reads, with attribution derived from
/// the tasks and `pilots` as the service derives it.
fn scan_facts(
    now: DateTime<Utc>,
    tasks: Vec<TaskFacts>,
    invocations: Vec<InvocationAccountingFact>,
    launches: Vec<AgentExecutionRecord>,
    pilots: Vec<Graded>,
) -> AgentFacts {
    let (charges, credits) = attribution::attribute(&tasks, &[], &pilots);
    AgentFacts {
        invocations: invocations.into(),
        launches: launches.into(),
        computed_at: now,
        tasks_scanned: tasks.len(),
        tasks,
        charges,
        credits,
        run_costs: BTreeMap::new(),
        regressions: Vec::new(),
        landed_lines: BTreeMap::new(),
        performance_types: Vec::new(),
        pilots,
    }
}

fn scan_tasks(now: DateTime<Utc>, tasks: Vec<TaskFacts>) -> AgentFacts {
    scan_facts(now, tasks, Vec::new(), Vec::new(), Vec::new())
}

fn week_query(min_support: usize) -> AgentQuery {
    AgentQuery::parse(Some("7d"), Some(min_support)).unwrap()
}

fn role_scorecard(facts: &AgentFacts, role: AgentRole) -> Value {
    agent_scorecard(facts, role, &week_query(1), Duration::ZERO)
}

fn implementer_attempt(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    crew: &str,
    model: &str,
    run_id: Option<&str>,
) -> Attempt {
    let mut attempt = Attempt {
        start,
        end: Some(end),
        runner: Default::default(),
        job: true,
        local: true,
        implementer: true,
        tags: Vec::new(),
    };
    attempt.runner.crew = Some(crew.into());
    attempt.runner.model = Some(model.into());
    attempt.runner.run_id = run_id.map(Into::into);
    attempt
}

fn review_of(
    attempt_id: &str,
    activity_id: Option<&str>,
    at: DateTime<Utc>,
    verdict: ReviewVerdict,
) -> Review {
    Review {
        attempt_id: attempt_id.into(),
        activity_id: activity_id.map(Into::into),
        at,
        verdict,
        reviewer: Default::default(),
        findings: Vec::new(),
        reviewed: None,
        final_candidate: None,
        seconds: None,
        fix_failed: false,
        infrastructure: false,
        outcome: RejectOutcome::Pending,
    }
}

fn settlement(activity_id: &str, at: DateTime<Utc>) -> Review {
    let mut review = review_of(activity_id, Some(activity_id), at, ReviewVerdict::Accept);
    review.reviewer.crew = Some("crew".into());
    review.reviewer.model = Some("rev-model".into());
    review
}

fn invocation(
    id: i64,
    (run_id, task_id): (&str, &str),
    (activity_id, model): (&str, &str),
    execution: Option<AgentExecutionIdentity>,
    duration_ms: u64,
    at: DateTime<Utc>,
) -> InvocationAccountingFact {
    InvocationAccountingFact {
        execution,
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

fn identity(crew: &str, effort: Option<ReasoningEffort>) -> AgentExecutionIdentity {
    AgentExecutionIdentity {
        crew: Some(crew.into()),
        reasoning_effort: effort,
    }
}

fn plus_minutes(base: DateTime<Utc>, minutes: i64) -> DateTime<Utc> {
    base + chrono::Duration::minutes(minutes)
}

fn minimal_task(id: &str, at: DateTime<Utc>) -> TaskFacts {
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
        execution: ExecutionMode::Managed,
        landed_at: Some(at),
        records: Vec::new(),
        reviews: Vec::new(),
        filed: None,
        warnings: Default::default(),
    }
}

fn in_review(id: &str, at: DateTime<Utc>, attempts: Vec<Attempt>) -> TaskFacts {
    TaskFacts {
        status: TaskStatus::Review,
        landed: false,
        landed_at: None,
        attempts,
        final_attempt: Some(0),
        ..minimal_task(id, at)
    }
}

#[test]
fn sparse_tier_scorecard_retains_own_evidence_and_conserves_fleet() {
    use TaskComplexity::{Hard, Low, Medium, XHard};
    // The old implementation gave the rare crew 9.09 expected landings.
    // Changing only its own outcomes must change that expectation, rather
    // than replacing it wholesale with the adjacent tier's rate.
    for (landed, accepted, expected) in [(0, 0, 3.75), (10, 10, 6.25)] {
        let payload = scorecard(&[
            ("common", Some(Medium), 100, 100, 60, 60),
            ("adjacent", Some(Hard), 30, 15, 10, 5),
            ("rare", Some(XHard), 10, landed, 10, accepted),
        ]);
        let rare = row(&payload, "rare");
        for term in ["weighted_landed", "first_pass_accepts"] {
            assert_eq!(
                rare["expected"][term],
                json!(expected),
                "{landed}/{accepted}: {term}"
            );
        }
        if landed == 0 {
            assert_eq!(
                row(&payload, "common")["expected"]["weighted_landed"],
                json!(100)
            );
            let adjustment = &adjustments(rare)[0];
            assert_eq!(adjustment["tier"], "xhard");
            assert_eq!(adjustment["adjusted"], true);
            assert_eq!(adjustment["reference"], "hard");
            assert_eq!(adjustment["reference_tiers"], json!(["hard"]));
            assert_eq!(
                adjustment["terms"]["weighted_landed"]["own_weight"],
                json!(0.25)
            );
        }
        assert_conserved(&payload);
    }

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

#[test]
fn implementer_rows_group_by_model_and_recorded_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Medium};
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    // (crew, run, invocation effort): three medium tasks (one also carrying a
    // shorter high invocation in the same run), two high, one provider
    // default, one with no invocation at all, and one medium task from a
    // second crew on the same model.
    let units = [
        ("opus", Some(("run-m1", Some(Medium)))),
        ("opus", Some(("run-m2", Some(Medium)))),
        ("opus", Some(("run-m3", Some(Medium)))),
        ("opus", Some(("run-h1", Some(High)))),
        ("opus", Some(("run-h2", Some(High)))),
        ("opus", Some(("run-p1", None))),
        ("opus", None),
        ("other", Some(("run-o1", Some(Medium)))),
    ];
    let mut tasks = Vec::new();
    let mut invocations = Vec::new();
    for (index, (crew, run)) in units.iter().enumerate() {
        let task_id = format!("effort-{index}");
        let run_id = run.map(|(run_id, _)| run_id);
        let attempt = implementer_attempt(start, now, crew, "m1", run_id);
        if let Some((run_id, effort)) = run {
            invocations.push(invocation(
                index as i64 * 2 + 1,
                (run_id, &task_id),
                ("implement_one", "m1"),
                Some(identity(crew, *effort)),
                60_000,
                now,
            ));
            if *run_id == "run-m3" {
                // A shorter high-effort invocation in the same run does not
                // move the unit: the longest (main) invocation decides.
                invocations.push(invocation(
                    999,
                    (run_id, &task_id),
                    ("implement_one", "m1"),
                    Some(identity(crew, Some(High))),
                    1_000,
                    now,
                ));
            }
        }
        tasks.push(in_review(&task_id, start, vec![attempt]));
    }
    let facts = scan_facts(now, tasks, invocations, Vec::new(), Vec::new());
    let payload = role_scorecard(&facts, AgentRole::Implementers);
    let rows = payload["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4, "one row per (model, recorded effort)");
    let medium = labelled_row(&payload, "m1 @ medium");
    assert_eq!(medium["units"], json!(4));
    assert_eq!(medium["reasoning_effort"], json!("medium"));
    assert_eq!(medium["crews"], json!(["opus", "other"]));
    assert_eq!(labelled_row(&payload, "m1 @ high")["units"], json!(2));
    let provider = labelled_row(&payload, "m1 @ provider default");
    assert_eq!(provider["units"], json!(1));
    assert!(provider["reasoning_effort"].is_null());
    let unknown = labelled_row(&payload, "m1 @ effort unknown");
    assert_eq!(unknown["units"], json!(1));
    assert_eq!(unknown["reasoning_effort"], json!("unknown"));
}

fn launch(
    id: i64,
    run_id: &str,
    activity_id: &str,
    model: &str,
    effort: Option<ReasoningEffort>,
    at: DateTime<Utc>,
) -> AgentExecutionRecord {
    AgentExecutionRecord {
        id,
        run_id: run_id.into(),
        activity_id: activity_id.into(),
        provider: "fixture".into(),
        model: Some(model.into()),
        identity: identity("opus", effort),
        started_at: at,
        backfill: None,
    }
}

#[test]
fn implementer_rows_use_launch_effort_when_invocation_never_completed() {
    use orbit_types::identity::ReasoningEffort::{High, Low, Medium};
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    // launch-only: launched but never completed; completed: a completed
    // medium invocation wins over a later high launch; neither: launches
    // only for another model or another activity stay unknown.
    let mut tasks = Vec::new();
    for (task_id, run_id) in [
        ("launch-only", "run-launch"),
        ("completed", "run-completed"),
        ("neither", "run-neither"),
    ] {
        let attempt = implementer_attempt(start, now, "opus", "m1", Some(run_id));
        tasks.push(in_review(task_id, start, vec![attempt]));
    }
    let invocations = vec![invocation(
        1,
        ("run-completed", "completed"),
        ("implement_one", "m1"),
        Some(identity("opus", Some(Medium))),
        60_000,
        now,
    )];
    // (id, run, activity, model, effort, minutes after start). The latest
    // matching launch decides for the unfinished attempt. Later launches for
    // another model or another activity are not this unit's evidence.
    let launches = [
        (1, "run-launch", "implement_one", "m1", Some(Low), 0),
        (2, "run-launch", "implement_one", "m1", Some(High), 1),
        (3, "run-launch", "implement_one", "m2", Some(Medium), 2),
        (4, "run-launch", "review", "m1", Some(Medium), 3),
        (5, "run-completed", "implement_one", "m1", Some(High), 1),
        (6, "run-neither", "implement_one", "m2", Some(High), 0),
        (7, "run-neither", "review", "m1", Some(High), 0),
    ]
    .map(|(id, run_id, activity_id, model, effort, minutes)| {
        launch(
            id,
            run_id,
            activity_id,
            model,
            effort,
            plus_minutes(start, minutes),
        )
    });
    let facts = scan_facts(now, tasks, invocations, launches.into(), Vec::new());
    let payload = role_scorecard(&facts, AgentRole::Implementers);
    let rows = payload["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "one row per attributed effort: {payload}");
    assert_eq!(units_of(&payload, "m1 @ high"), json!(1));
    assert_eq!(units_of(&payload, "m1 @ medium"), json!(1));
    assert_eq!(units_of(&payload, "m1 @ effort unknown"), json!(1));
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
            let id = format!("{model}-{index}");
            let mut review = review_of(&id, None, at, *verdict);
            review.reviewer.crew = Some("review-crew".into());
            review.reviewer.model = Some((*model).into());
            tasks.push(TaskFacts {
                status: TaskStatus::Review,
                landed: false,
                landed_at: None,
                reviews: vec![review],
                ..minimal_task(&id, at)
            });
        }
    }
    let facts = scan_tasks(now, tasks);
    let payload = agent_scorecard(&facts, AgentRole::Reviewers, &week_query(5), Duration::ZERO);
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
    let small = model_row(&payload, "r-small");
    assert!(small["rank"].is_null(), "below-support rows stay unranked");
    assert_eq!(small["below_support"], json!(true));
}

fn pilot_run(run_id: &str, model: &str, activity_id: &str) -> PilotRun {
    PilotRun {
        run_id: run_id.into(),
        crew: Some("crew".into()),
        model: Some(model.into()),
        activity_id: Some(activity_id.into()),
        seconds: None,
        tokens: None,
    }
}

fn pilot_assessment(
    task_id: &str,
    at: DateTime<Utc>,
    run: PilotRun,
    file: &str,
) -> PilotAssessment {
    PilotAssessment {
        task_id: task_id.into(),
        at,
        run: Some(run),
        after: vec![format!("file:{file}")],
        author: Vec::new(),
        landed: BTreeSet::from([file.to_string()]),
        contested: Vec::new(),
    }
}

fn grade(assessments: Vec<PilotAssessment>) -> Vec<Graded> {
    grade_all(PilotEvidence {
        assessments,
        waits: Vec::new(),
    })
}

#[test]
fn pilot_rows_keep_each_rounds_own_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Medium};
    let now = Utc::now();
    let at = plus_minutes(now, -30);
    // The initial round's 10s/high must not borrow the repair's 60s/medium.
    // The orphan's worker has no matching invocation, so stays unknown.
    // (name, run, receipt activity, invocation activity, effort, duration).
    let pilot = "pilot";
    let repair = "repair_pilot";
    let rounds = [
        ("initial", "run-pilot", pilot, pilot, High, 10_000),
        ("repair", "run-pilot", repair, repair, Medium, 60_000),
        ("orphan", "run-orphan", repair, pilot, High, 60_000),
    ];
    let mut tasks = Vec::new();
    let mut assessments = Vec::new();
    let mut invocations = Vec::new();
    for (index, (name, run, receipt, activity, effort, duration)) in rounds.into_iter().enumerate()
    {
        let task = format!("pilot-{name}");
        let model = format!("{name}-model");
        let file = ["src/a.rs", "src/b.rs", "src/c.rs"][index];
        tasks.push(minimal_task(&task, at));
        assessments.push(pilot_assessment(
            &task,
            at,
            pilot_run(run, &model, receipt),
            file,
        ));
        invocations.push(invocation(
            index as i64 + 1,
            (run, &task),
            (activity, &model),
            Some(identity("crew", Some(effort))),
            duration,
            at,
        ));
    }
    let graded = grade(assessments);
    let facts = scan_facts(now, tasks, invocations, Vec::new(), graded);
    let payload = role_scorecard(&facts, AgentRole::Pilots);
    assert_eq!(units_of(&payload, "initial-model @ high"), json!(1));
    assert_eq!(units_of(&payload, "repair-model @ medium"), json!(1));
    assert_eq!(
        units_of(&payload, "orphan-model @ effort unknown"),
        json!(1)
    );
}

/// Warning intervals must resample settled warnings, including tasks outside
/// the assessed cohort.
#[test]
fn pilot_interval_resamples_settled_warnings_on_unassessed_tasks() {
    use crate::application::agents::roles::pilot_warnings::{Assessment, Outcome, Warnings};
    let now = Utc::now();
    let at = plus_minutes(now, -30);
    // Ten landed tasks the pilot assessed with no lift over the filer.
    let mut tasks: Vec<TaskFacts> = (0..10)
        .map(|index| minimal_task(&format!("assessed-{index}"), at))
        .collect();
    let graded = grade(
        tasks
            .iter()
            .map(|task| PilotAssessment {
                author: vec!["file:src/a.rs".into()],
                ..pilot_assessment(
                    &task.id,
                    at,
                    pilot_run(&format!("run-{}", task.id), "pilot-model", "pilot"),
                    "src/a.rs",
                )
            })
            .collect(),
    );
    // Warnings that settled in the window on other tasks: some dropped, one
    // never landed, none in the assessed cohort.
    let settled = |outcomes: &[Outcome]| -> Vec<TaskFacts> {
        outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| {
                let mut task = minimal_task(&format!("other-{index}"), at);
                task.landed = index % 2 == 0;
                task.landed_at = task.landed.then_some(at);
                task.dropped = index % 2 == 1;
                task.status = if task.dropped {
                    TaskStatus::Rejected
                } else {
                    TaskStatus::Done
                };
                task.warnings = Warnings {
                    assessments: vec![Assessment {
                        at: at - chrono::Duration::days(2),
                        operation_id: format!("op-{index}"),
                        run_id: None,
                        held: Vec::new(),
                        outcome: outcome.clone(),
                        settled: Some((at, "settled".into())),
                        run: Some(pilot_run(
                            &format!("run-other-{index}"),
                            "pilot-model",
                            "pilot",
                        )),
                        mode: "full".into(),
                        escalated: false,
                    }],
                    triaged: Vec::new(),
                };
                task
            })
            .collect()
    };
    let scored = |tasks: Vec<TaskFacts>| {
        let facts = scan_facts(now, tasks, Vec::new(), Vec::new(), graded.clone());
        let payload = role_scorecard(&facts, AgentRole::Pilots);
        let rows = payload["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{payload}");
        rows[0].clone()
    };
    let band = |row: &Value| {
        (
            row["interval"]["p10"].as_f64().unwrap(),
            row["interval"]["p90"].as_f64().unwrap(),
        )
    };

    // Ten confirmed warnings on other tasks: the score is 3 x 100 and every
    // draw of the settled cohort is all-confirmed, so the interval is that
    // score, not the zero the assessed-only draws produced.
    let mut confirmed_tasks = tasks.clone();
    confirmed_tasks.extend(settled(&vec![Outcome::Confirmed; 10]));
    let confirmed = scored(confirmed_tasks);
    assert_eq!(confirmed["units"], json!(10));
    assert_eq!(confirmed["rank"], json!(1));
    assert_eq!(confirmed["raw"].as_f64(), Some(300.0));
    assert_eq!(confirmed["score"].as_f64(), Some(300.0));
    assert_eq!(confirmed["term_units"]["confirmed_warnings"], json!(10));
    assert_eq!(band(&confirmed), (300.0, 300.0), "{confirmed}");

    // Seven confirmed and three dismissed: 3 x 70 - 30 = 180. The settled
    // cohort's own denominator is resampled, so the band is real, brackets
    // the score and repeats exactly.
    let mut mixed_tasks = tasks.clone();
    let mut outcomes = vec![Outcome::Confirmed; 7];
    outcomes.extend(vec![Outcome::Dismissed; 3]);
    mixed_tasks.extend(settled(&outcomes));
    let mixed = scored(mixed_tasks.clone());
    assert_eq!(mixed["raw"].as_f64(), Some(180.0));
    assert_eq!(mixed["score"].as_f64(), Some(180.0));
    let (low, high) = band(&mixed);
    assert!(
        low > 0.0 && low < 180.0 && high > 180.0,
        "the band must bracket the score: {mixed}"
    );
    assert_eq!(band(&scored(mixed_tasks)), (low, high));

    // Fewer than the minimum settled: the warning terms stay unscored, the
    // score is the lift's own (0) and the interval does not invent warnings.
    let mut thin_tasks = std::mem::take(&mut tasks);
    thin_tasks.extend(settled(&vec![Outcome::Confirmed; 9]));
    let thin = scored(thin_tasks);
    assert_eq!(thin["raw"].as_f64(), Some(0.0));
    assert_eq!(band(&thin), (0.0, 0.0), "{thin}");
}

/// Each settlement keeps its invocation's effort, including re-reviews whose
/// comments repeat the first lead and bind to the gate's `review` step.
#[test]
fn reviewer_rows_keep_each_settlements_own_effort() {
    use orbit_types::identity::ReasoningEffort::{High, Low, Medium};
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    // Each case: settlements (activity, minute), invocations (activity,
    // effort, duration_ms, minute), labels that need one unit, and labels
    // that must not appear. Minutes are after `start`.
    let cases = [
        (
            "before-pr/landing",
            vec![("review", 20), ("landing_review", 50)],
            vec![
                ("review", High, 10_000, 19),
                ("landing_review", Medium, 60_000, 49),
            ],
            vec!["rev-model @ high", "rev-model @ medium"],
            vec![],
        ),
        (
            "re-review",
            vec![("review", 20), ("review", 35), ("review", 50)],
            vec![
                ("review", High, 10_000, 19),
                ("re_review", Medium, 60_000, 34),
                ("re_review_2", Low, 30_000, 49),
            ],
            vec!["rev-model @ high", "rev-model @ medium", "rev-model @ low"],
            vec!["rev-model @ effort unknown"],
        ),
    ];
    for (case, settled, invoked, expected, absent) in cases {
        let task = TaskFacts {
            attempts: vec![implementer_attempt(
                start,
                now,
                "impl",
                "impl-model",
                Some("run-pr"),
            )],
            final_attempt: Some(0),
            reviews: settled
                .iter()
                .map(|&(activity, minute)| settlement(activity, plus_minutes(start, minute)))
                .collect(),
            ..minimal_task("reviewed-task", start)
        };
        let invocations = invoked
            .iter()
            .enumerate()
            .map(|(index, &(activity, effort, duration_ms, minute))| {
                invocation(
                    index as i64 + 1,
                    ("run-pr", "reviewed-task"),
                    (activity, "rev-model"),
                    Some(identity("crew", Some(effort))),
                    duration_ms,
                    plus_minutes(start, minute),
                )
            })
            .collect();
        let facts = scan_facts(now, vec![task], invocations, Vec::new(), Vec::new());
        let payload = role_scorecard(&facts, AgentRole::Reviewers);
        for label in expected {
            assert_eq!(
                units_of(&payload, label),
                json!(1),
                "{case}: {label}: {payload}"
            );
        }
        let rows = payload["rows"].as_array().unwrap();
        for label in absent {
            assert!(
                rows.iter().all(|row| row["label"] != label),
                "{case}: {label}: {payload}"
            );
        }
    }
}

#[test]
fn reviewer_interval_resamples_review_units_not_task_groups() {
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    let changed = BTreeSet::from(["src/lib.rs".to_string()]);
    let review = |id: &str, at| {
        let mut review = Review {
            findings: vec![Finding {
                severity: "low".into(),
                state: FindingState::Open,
                paths: changed.clone(),
                line: format!("finding {id}"),
            }],
            outcome: RejectOutcome::Judged {
                changed: changed.clone(),
                accepted: false,
            },
            ..review_of(id, None, at, ReviewVerdict::Reject)
        };
        review.reviewer.crew = Some("review-crew".into());
        review.reviewer.model = Some("review-model".into());
        review
    };
    let shared = {
        let mut task = minimal_task("shared-task", start);
        task.reviews = vec![
            review("review-one", plus_minutes(start, 10)),
            review("review-two", plus_minutes(start, 10)),
        ];
        scan_tasks(now, vec![task])
    };
    let split = {
        let mut first = minimal_task("first-task", start);
        first.reviews = vec![review("review-one", plus_minutes(start, 10))];
        let mut second = minimal_task("second-task", start);
        second.reviews = vec![review("review-two", plus_minutes(start, 20))];
        scan_tasks(now, vec![first, second])
    };
    let interval = |payload: &Value| model_row(payload, "review-model")["interval"].clone();
    let shared_payload = role_scorecard(&shared, AgentRole::Reviewers);
    assert!(interval(&shared_payload).is_object(), "{shared_payload}");
    assert_eq!(
        model_row(&shared_payload, "review-model")["units"],
        json!(2)
    );
    let repeated = role_scorecard(&shared, AgentRole::Reviewers);
    assert_eq!(interval(&shared_payload), interval(&repeated));
    assert_eq!(
        interval(&shared_payload),
        interval(&role_scorecard(&split, AgentRole::Reviewers)),
        "the same two review units must have the same interval whether they belong to one or two tasks"
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
    let attempt_at = |run_id: Option<&str>, at: DateTime<Utc>| {
        implementer_attempt(at, plus_minutes(at, 5), "opus", "m1", run_id)
    };
    let mut task = minimal_task("borrowed-effort", start);
    task.status = TaskStatus::Review;
    task.attempts = vec![
        attempt_at(None, start),
        attempt_at(Some("run-later"), plus_minutes(start, 30)),
    ];
    task.final_attempt = Some(1);
    let invocations = vec![invocation(
        1,
        ("run-later", "borrowed-effort"),
        ("implement_one", "m1"),
        Some(identity("crew", Some(High))),
        60_000,
        now,
    )];
    let facts = scan_facts(now, vec![task], invocations, Vec::new(), Vec::new());
    let payload = role_scorecard(&facts, AgentRole::Implementers);
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

/// Record the recovery decision just before its applier's transition.
fn decided(
    base: DateTime<Utc>,
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

fn started(base: DateTime<Utc>, minutes: i64) -> Value {
    json!({"at": plus_minutes(base, minutes), "by": "system", "event": "started", "from_status": "backlog", "to_status": "in_progress"})
}

fn to_review(base: DateTime<Utc>, minutes: i64) -> Value {
    json!({"at": plus_minutes(base, minutes), "by": "system", "event": "status_changed", "from_status": "in_progress", "to_status": "review"})
}

fn landed(base: DateTime<Utc>, minutes: i64) -> Value {
    json!({"at": plus_minutes(base, minutes), "by": "owner-landing", "event": "landing_completed", "from_status": "review", "to_status": "done"})
}

/// Escalation after rerun_ci cannot read the pull request's checks.
fn rerun_ci_escalation(base: DateTime<Utc>, minutes: i64, run: &str, fault: &str) -> [Value; 2] {
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
        json!({
            "at": plus_minutes(base, minutes),
            "by": "system",
            "event": "final_recovery_escalated",
            "note": format!("final recovery (run_id={run}) escalated: {reason}"),
            "from_status": "review",
            "to_status": "blocked",
            "decision": {"kind": "run_transition", "run_id": run},
        }),
    ]
}

fn operator_requeue(base: DateTime<Utc>, minutes: i64, run: &str) -> [Value; 2] {
    [
        decided(
            base,
            minutes,
            run,
            "operator_requeue",
            json!({"kind": "requeued"}),
            "unknown",
        ),
        json!({
            "at": plus_minutes(base, minutes),
            "by": "human:daniel",
            "event": "operator_requeued",
            "note": "requeued onto the published candidate: auto-fix found no defect; failed \
                     job was a ripgrep download timeout. Reran CI by hand: required checks \
                     green. Resume the candidate.",
            "from_status": "blocked",
            "to_status": "backlog",
        }),
    ]
}

fn failed_then_requeued(base: DateTime<Utc>, minutes: i64, run: &str, fault: &str) -> [Value; 3] {
    [
        json!({
            "at": plus_minutes(base, minutes),
            "by": "system",
            "event": "workflow_run_failed",
            "note": format!("workflow failed: step=validate, run_id={run}, error_code=validation_failed"),
            "from_status": "in_progress",
            "to_status": "blocked",
        }),
        decided(
            base,
            minutes + 5,
            run,
            "requeue",
            json!({"kind": "requeued"}),
            fault,
        ),
        json!({
            "at": plus_minutes(base, minutes + 5),
            "by": "system",
            "event": "final_recovery_requeued",
            "note": format!("final recovery (run_id={run}): requeued for a fresh attempt"),
            "from_status": "blocked",
            "to_status": "backlog",
        }),
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
    facts.execution = ExecutionMode::Managed;
    facts
}

#[test]
fn memory_evidence_charges_implementers_only_for_judged_implementation_faults() {
    let base = Utc::now() - chrono::Duration::hours(6);
    let created = |minutes| json!({"at": plus_minutes(base, minutes), "by": "claude", "event": "created", "to_status": "proposed"});
    let mut infra = vec![created(0), started(base, 1), to_review(base, 15)];
    infra.extend(rerun_ci_escalation(base, 40, "jrun-a1", "unknown"));
    infra.extend(operator_requeue(base, 46, "jrun-a1"));
    infra.extend([
        json!({"at": plus_minutes(base, 47), "by": "hm_mac", "event": "pulled_by", "from_status": "backlog", "to_status": "in_progress", "note": "{}"}),
        json!({"at": plus_minutes(base, 56), "by": "hm_mac", "event": "claim_handed_off", "from_status": "in_progress", "to_status": "review"}),
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

    // A block by hand, requeued by hand, with nothing judging the work.
    let mut manual = vec![created(0), started(base, 1)];
    manual.extend([
        json!({"at": plus_minutes(base, 10), "by": "human:daniel", "event": "status_changed", "from_status": "in_progress", "to_status": "blocked"}),
        json!({"at": plus_minutes(base, 20), "by": "human:daniel", "event": "status_changed", "from_status": "blocked", "to_status": "backlog"}),
        started(base, 21),
        to_review(base, 30),
        landed(base, 40),
    ]);

    // An escalation written before faults were recorded.
    let mut unjudged = vec![created(0), started(base, 1), to_review(base, 15)];
    unjudged.push(rerun_ci_escalation(base, 40, "jrun-d1", "unknown")[1].clone());
    unjudged.extend(operator_requeue(base, 46, "jrun-d1"));
    unjudged.extend([started(base, 50), to_review(base, 60), landed(base, 70)]);

    let mut tasks = vec![
        observed_task("infra", "fixture-model", infra),
        observed_task("mixed", "fixture-model", mixed),
        observed_task("manual", "fixture-model", manual),
        observed_task("unjudged", "fixture-model", unjudged),
    ];
    // The same retry sequence, once for a specification fault and once for
    // another model's implementation fault (which must stay in its pack).
    for (id, model, run, fault) in [
        ("spec", "fixture-model", "jrun-c1", "specification"),
        ("other", "other-model", "jrun-e1", "implementer"),
    ] {
        let mut history = vec![created(0), started(base, 1)];
        history.extend(failed_then_requeued(base, 10, run, fault));
        history.extend([started(base, 20), to_review(base, 30), landed(base, 40)]);
        tasks.push(observed_task(id, model, history));
    }
    let facts = scan_tasks(Utc::now(), tasks);

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
    // ORB-23194: a download timeout, unknown-mergeability refusal and unchanged
    // candidate requeue are not implementation faults.
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

/// A labelled unit's run id and, when it completed an invocation, whether
/// that invocation recorded a dispatch identity and at which effort.
type LabelledRun = Option<(&'static str, Option<(bool, Option<ReasoningEffort>)>)>;

/// One implementer unit for the crew-label fixtures: task id, crew label,
/// its run ([`LabelledRun`]) and the effort a follower reported for it.
type LabelledUnit = (
    &'static str,
    &'static str,
    LabelledRun,
    Option<Option<String>>,
);

fn labelled_facts(units: Vec<LabelledUnit>) -> AgentFacts {
    let now = Utc::now();
    let start = now - chrono::Duration::hours(1);
    let mut tasks = Vec::new();
    let mut invocations = Vec::new();
    for (index, (task_id, crew, run, reported)) in units.into_iter().enumerate() {
        let run_id = run.map(|(run_id, _)| run_id);
        let mut attempt = implementer_attempt(start, now, crew, "m1", run_id);
        attempt.runner.reported_effort = reported;
        if let Some((run_id, Some((identified, effort)))) = run {
            invocations.push(invocation(
                index as i64 + 1,
                (run_id, task_id),
                ("implement_one", "m1"),
                identified.then(|| identity(crew, effort)),
                60_000,
                now,
            ));
        }
        tasks.push(TaskFacts {
            complexity: Some(TaskComplexity::Low),
            landed_at: Some(now),
            attempts: vec![attempt],
            final_attempt: Some(0),
            ..minimal_task(task_id, start)
        });
    }
    scan_facts(now, tasks, invocations, Vec::new(), Vec::new())
}

/// [ORB-24150] Without recorded effort, `<crew>@<effort>` supplies the scorecard,
/// detail and ranking effort; a bare crew stays unknown. Previously both units
/// were unknown.
#[test]
fn crew_label_effort_counts_when_no_effort_is_recorded() {
    use super::super::{RankQuery, RankSeasons, agent_detail_with_effort, agent_rank};
    let facts = labelled_facts(vec![
        // No run at all: no invocation, launch record or report.
        ("label-no-run", "opus@medium", None, None),
        // A completed invocation with no dispatch identity.
        (
            "label-old-invocation",
            "opus@medium",
            Some(("run-old", Some((false, None)))),
            None,
        ),
        ("bare", "opus", None, None),
    ]);
    let query = week_query(1);
    let payload = agent_scorecard(&facts, AgentRole::Implementers, &query, Duration::ZERO);
    let medium = labelled_row(&payload, "m1 @ medium");
    assert_eq!(medium["units"], json!(2), "{payload}");
    assert_eq!(medium["effort_sources"], json!({"crew_label": 2}));
    assert_eq!(medium["crews"], json!(["opus@medium"]));
    let unknown = labelled_row(&payload, "m1 @ effort unknown");
    assert_eq!(
        unknown["units"],
        json!(1),
        "a bare crew label names no effort"
    );
    assert_eq!(unknown["effort_sources"], json!({"unrecorded": 1}));

    let detail = agent_detail_with_effort(
        &facts,
        AgentRole::Implementers,
        &query,
        "opus@medium",
        Some("m1"),
        Some("medium"),
        Duration::ZERO,
    )
    .unwrap();
    assert_eq!(detail["row"]["effort_sources"], json!({"crew_label": 2}));

    let live = RankQuery::parse(None, None, None, None, true).unwrap();
    let rank = agent_rank(&facts, &live, &RankSeasons::default(), Duration::ZERO);
    let split = model_row(&rank, "m1")["effort_split"].clone();
    assert_eq!(
        split,
        json!([{
            "crew": "opus@medium",
            "model": "m1",
            "reasoning_effort": "medium",
            "invocations": 1,
            "tasks": 1,
            "duration_ms": 60_000,
            "input_tokens": 0,
            "output_tokens": 0,
            "provider_cost_usd": 0.0,
            "provider_cost_count": 0,
        }]),
        "the identity-less invocation counts under its run's crew-label effort"
    );
}

/// [ORB-24150] A recorded effort — invocation, launch record or follower
/// report — wins over a crew label that names a different one.
#[test]
fn recorded_effort_wins_over_a_crew_label_effort() {
    use orbit_types::identity::ReasoningEffort::Medium;
    let facts = labelled_facts(vec![
        (
            "invocation-medium",
            "opus@high",
            Some(("run-recorded", Some((true, Some(Medium))))),
            None,
        ),
        ("reported-low", "opus@high", None, Some(Some("low".into()))),
    ]);
    let payload = role_scorecard(&facts, AgentRole::Implementers);
    let rows = payload["rows"].as_array().unwrap();
    let labels: BTreeSet<&str> = rows
        .iter()
        .map(|row| row["label"].as_str().unwrap())
        .collect();
    assert_eq!(
        labels,
        BTreeSet::from(["m1 @ low", "m1 @ medium"]),
        "{payload}"
    );
    assert_eq!(
        labelled_row(&payload, "m1 @ medium")["effort_sources"],
        json!({"invocation": 1})
    );
    assert_eq!(
        labelled_row(&payload, "m1 @ low")["effort_sources"],
        json!({"follower_report": 1})
    );
}

/// [ORB-24640] Cache reuse preserves a pulled task's provider-default report,
/// overriding a conflicting crew suffix and staying distinct from missing and
/// named efforts. Previously a cached null lost the report and used the crew.
#[test]
fn provider_default_report_survives_stored_facts_reuse() {
    let cold = labelled_facts(vec![
        // Conflicting crew suffix: the reported default must still win warm.
        ("pulled-default", "opus@high", None, Some(None)),
        ("pulled-missing", "opus", None, None),
        ("pulled-named", "opus", None, Some(Some("low".into()))),
    ]);
    // The stored-facts cache round trip `StoredFacts::keep`/`entry` perform.
    let warm_tasks: Vec<TaskFacts> = cold
        .tasks
        .iter()
        .map(|task| serde_json::from_str(&serde_json::to_string(task).unwrap()).unwrap())
        .collect();
    let warm = scan_tasks(cold.computed_at, warm_tasks);
    let cold_payload = role_scorecard(&cold, AgentRole::Implementers);
    let warm_payload = role_scorecard(&warm, AgentRole::Implementers);
    assert_eq!(
        warm_payload["rows"], cold_payload["rows"],
        "an unchanged warm scan must preserve the cold scan's grouping"
    );
    assert_eq!(
        labelled_row(&warm_payload, "m1 @ provider default")["effort_sources"],
        json!({"follower_report": 1}),
        "the provider-default report survives cache reuse: {warm_payload}"
    );
    assert_eq!(
        labelled_row(&warm_payload, "m1 @ effort unknown")["effort_sources"],
        json!({"unrecorded": 1}),
        "a missing report stays distinct: {warm_payload}"
    );
    assert_eq!(
        labelled_row(&warm_payload, "m1 @ low")["effort_sources"],
        json!({"follower_report": 1}),
        "a named effort stays distinct: {warm_payload}"
    );
}
