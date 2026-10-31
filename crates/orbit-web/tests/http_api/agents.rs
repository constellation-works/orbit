//! The agent scorecards (`/api/agents/*`) through the runtime API and the
//! HTTP API, over fixture tasks where two crews implemented one task, a
//! machine pulled another, a reviewer settled reviews and four regressions
//! name their escapes.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::agents::{AgentRole, Owner, Problem};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{JobRun, JobRunState, TaskStatus};
use orbit_types::task::{
    DESCRIPTION_DOCUMENT, DefectSeverity, RegressionClassification, RegressionEscape,
    RegressionNature, SeveritySource, TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME,
    TaskComplexity, TaskEventRowV2, TaskOrigin, TaskRelation, TaskRelationType,
    task_fields_updated_note,
};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

pub(super) const WS: &str = "workspace=ws_http_fixture";

pub(super) struct Owned<'a> {
    pub(super) title: &'a str,
    pub(super) orchestrator: Option<&'a str>,
    pub(super) origin: Option<TaskOrigin>,
    pub(super) complexity: TaskComplexity,
    pub(super) status: TaskStatus,
    pub(super) relations: Vec<TaskRelation>,
}

pub(super) fn add(fixture: &Fixture, task: Owned<'_>) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: task.title.into(),
            description: "agent scorecard fixture".into(),
            orchestrator: task.orchestrator.map(str::to_string),
            origin: task.origin,
            complexity: task.complexity,
            status: Some(TaskStatus::Backlog),
            relations: task.relations,
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    if task.status != TaskStatus::Backlog {
        fixture
            .runtime
            .force_update_task_with_identity(
                &id,
                TaskUpdateParams {
                    status: Some(task.status),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
    }
    id
}

pub(super) fn regression(
    fixture: &Fixture,
    culprit: &str,
    nature: RegressionNature,
    escape: RegressionEscape,
) -> String {
    rated_regression(fixture, culprit, nature, escape, DefectSeverity::Low)
}

pub(super) fn rated_regression(
    fixture: &Fixture,
    culprit: &str,
    nature: RegressionNature,
    escape: RegressionEscape,
    severity: DefectSeverity,
) -> String {
    add(
        fixture,
        Owned {
            title: "regression",
            orchestrator: None,
            origin: None,
            complexity: TaskComplexity::Low,
            status: TaskStatus::Backlog,
            relations: vec![TaskRelation {
                relation_type: TaskRelationType::RegressionFrom,
                target: culprit.parse().unwrap(),
                regression: Some(RegressionClassification {
                    nature,
                    escape,
                    severity,
                    severity_source: SeveritySource::Filer,
                    sites: Vec::new(),
                }),
            }],
        },
    )
}

pub(super) fn bundle_dir(root: &Path, task_id: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == task_id)
            && path.join(TASK_EVENTS_FILE_NAME).is_file()
        {
            return Some(path);
        }
        if let Some(found) = bundle_dir(&path, task_id) {
            return Some(found);
        }
    }
    None
}

/// Append one history row to a disposable fixture task, as a lifecycle writer
/// would: runs, pulls, blocks and requeues have no public operator entry
/// point that a fixture can drive without a crew.
pub(super) fn append(
    fixture: &Fixture,
    task_id: &str,
    at: DateTime<Utc>,
    event: &str,
    note: Option<&str>,
    to_status: Option<TaskStatus>,
) {
    let dir = bundle_dir(fixture.path("").as_path(), task_id).expect("fixture task bundle");
    let path = dir.join(TASK_EVENTS_FILE_NAME);
    let rows = fs::read_to_string(&path).unwrap().lines().count();
    let row = TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", rows + 1),
        at,
        by: "system".into(),
        event_type: event.into(),
        note: note.map(str::to_string),
        from_status: None,
        to_status,
        operation_id: None,
        decision: None,
    };
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&row).unwrap()).unwrap();
}

/// A finished implementation run for `task_id`, started a second before the
/// history entry that starts its attempt, as the pipeline records it.
pub(super) fn implementation_run(
    fixture: &Fixture,
    run_id: &str,
    task_id: &str,
    crew: &str,
    model: Option<&str>,
    started: DateTime<Utc>,
    finished: DateTime<Utc>,
) {
    fixture.save_run(&JobRun {
        run_id: run_id.into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: started,
        created_at: started,
        started_at: Some(started),
        finished_at: Some(finished),
        duration_ms: None,
        pid: None,
        pid_start_time: None,
        input: Some(json!({"task_ids": [task_id]})),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: Some(crew.into()),
        crew_model: model.map(str::to_string),
        steps: Vec::new(),
        executed_on: None,
    });
}

/// Replace a fixture task's tags through the public update path, as an
/// operator retagging it now — after any attempt the fixture started.
pub(super) fn retag(fixture: &Fixture, task_id: &str, tags: Vec<String>) {
    fixture
        .runtime
        .update_task_as_human(
            task_id,
            TaskUpdateParams {
                tags: Some(tags),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
}

pub(super) fn comment(fixture: &Fixture, task_id: &str, message: &str) {
    fixture
        .runtime
        .update_task_as_human(
            task_id,
            TaskUpdateParams {
                comment: Some(message.into()),
                ..Default::default()
            },
            "fixture".into(),
        )
        .unwrap();
}

fn operations(trace: &[Value], name: &str) -> usize {
    trace
        .iter()
        .filter(|event| event["span"]["name"] == name)
        .count()
}

pub(super) fn as_f64(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a number: {value}"))
}

/// Costs and durations describe work without changing its score;
/// the same crew's models remain separate through runtime and HTTP reads.
#[test]
fn implementers_report_cost_duration_tiers_and_first_reviews_without_scoring_cost() {
    isolated(
        "agents::implementers_report_cost_duration_tiers_and_first_reviews_without_scoring_cost",
        || {
            use orbit_core::InvocationInsertParams;
            use orbit_core::application::agents::{AgentQuery, agent_detail, agent_scorecard};
            use orbit_types::telemetry::InvocationTrace;
            let fixture = Fixture::new();
            let now = Utc::now();
            let mut ids = Vec::new();
            for (index, verdict) in ["accept", "accept_with_fixes"].iter().enumerate() {
                let id = add(
                    &fixture,
                    Owned {
                        title: verdict,
                        orchestrator: None,
                        origin: None,
                        complexity: TaskComplexity::Medium,
                        status: TaskStatus::Done,
                        relations: vec![],
                    },
                );
                let run = format!("jrun-model-{index}");
                let model = format!("model-{index}");
                let start = now - Duration::hours(3);
                implementation_run(
                    &fixture,
                    &run,
                    &id,
                    "crew",
                    Some(&model),
                    start,
                    now - Duration::hours(2),
                );
                append(
                    &fixture,
                    &id,
                    start,
                    "started",
                    None,
                    Some(TaskStatus::InProgress),
                );
                append(
                    &fixture,
                    &id,
                    now - Duration::hours(2),
                    "status_changed",
                    None,
                    Some(TaskStatus::Done),
                );
                comment(
                    &fixture,
                    &id,
                    &format!(
                        "before-PR review settled attempt `rv-{index}`: verdict **{verdict}**\n- Reviewer: crew `reviewer` (codex / review-model)"
                    ),
                );
                fixture
                    .runtime
                    .insert_invocation_trace_record(&InvocationInsertParams {
                        agent_execution_id: Some(
                            fixture
                                .runtime
                                .sqlite_store()
                                .unwrap()
                                .begin_agent_execution(
                                    fixture.runtime.stores().jobs().workspace_id(),
                                    &run,
                                    "implement_one",
                                    "codex",
                                    Some(&model),
                                    &orbit_types::telemetry::AgentExecutionIdentity {
                                        crew: Some("crew".into()),
                                        reasoning_effort: (index == 0).then_some(
                                            orbit_types::identity::ReasoningEffort::High,
                                        ),
                                    },
                                )
                                .unwrap(),
                        ),
                        job_run_id: run,
                        account: None,
                        activity_id: "implement_one".into(),
                        agent: "codex".into(),
                        provider: None,
                        model: Some(model),
                        task_ids: vec![id.clone()],
                        trace: InvocationTrace {
                            provider_cost_usd: Some(6.0),
                            ..Default::default()
                        },
                    })
                    .unwrap();
                ids.push(id);
            }
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            assert_eq!(card["status"], "ok");
            let rows = card["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 2, "models are distinct even under one crew");
            let stat = |row: &Value, key: &str| {
                row["stats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|stat| stat["key"] == key)
                    .unwrap()["value"]
                    .clone()
            };
            let accepted = rows.iter().find(|row| row["model"] == "model-0").unwrap();
            let repaired = rows.iter().find(|row| row["model"] == "model-1").unwrap();
            assert_eq!(accepted["counts"]["first_pass_accepts"], 1);
            assert_eq!(repaired["counts"]["reviewer_repairs"], 1);
            assert_eq!(repaired["counts"]["crew_rework"], 0);
            assert!(
                as_f64(&accepted["score"]) > as_f64(&repaired["score"]),
                "an accepted candidate outranks one repaired by its reviewer"
            );
            for row in rows {
                let weighted = as_f64(&row["counts"]["weighted_landed"]);
                assert_eq!(stat(row, "cost_per_weighted_landed"), json!(6.0 / weighted));
                assert_eq!(stat(row, "median_run_seconds"), 3600);
                assert_eq!(row["counts"]["weighted_landed"], 1);
                assert_eq!(row["expected"]["first_pass_accepts"], json!(0.5));
                assert_eq!(row["expected"]["reviewer_repairs"], json!(0.5));
            }
            fixture
                .runtime
                .insert_invocation_trace_record(&InvocationInsertParams {
                    agent_execution_id: Some(
                        fixture
                            .runtime
                            .sqlite_store()
                            .unwrap()
                            .begin_agent_execution(
                                fixture.runtime.stores().jobs().workspace_id(),
                                "jrun-model-0",
                                "implement_one",
                                "codex",
                                Some("model-0"),
                                &orbit_types::telemetry::AgentExecutionIdentity {
                                    crew: Some("crew".into()),
                                    reasoning_effort: Some(
                                        orbit_types::identity::ReasoningEffort::Xhigh,
                                    ),
                                },
                            )
                            .unwrap(),
                    ),
                    job_run_id: "jrun-model-0".into(),
                    account: None,
                    activity_id: "implement_one".into(),
                    agent: "codex".into(),
                    provider: None,
                    model: Some("model-0".into()),
                    task_ids: vec![ids[0].clone()],
                    trace: InvocationTrace {
                        provider_cost_usd: Some(12.0),
                        ..Default::default()
                    },
                })
                .unwrap();
            let next_facts = fixture.runtime.agent_scan().unwrap();
            let detail = agent_detail(
                &next_facts,
                AgentRole::Implementers,
                &query,
                "crew",
                Some("model-0"),
                std::time::Duration::ZERO,
            )
            .unwrap();
            assert_eq!(
                detail["row"]["score"], accepted["score"],
                "cost never enters the score"
            );
            assert_eq!(
                stat(&detail["row"], "cost_per_weighted_landed"),
                json!(18.0 / as_f64(&accepted["counts"]["weighted_landed"]))
            );
            let server = fixture.server(false);
            let http = json_ok(server.get(&format!(
                "/api/agents/implementers/detail?{WS}&crew=crew&model=model-0&min_support=1"
            )));
            assert_eq!(http["status"], "ok");
            assert_eq!(http["row"], detail["row"]);
            let efforts: std::collections::BTreeSet<_> = http["row"]["effort_breakdown"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["reasoning_effort"].to_string())
                .collect();
            assert_eq!(
                efforts,
                ["\"high\"".into(), "\"xhigh\"".into()]
                    .into_iter()
                    .collect()
            );
            assert_eq!(
                repaired["effort_breakdown"][0]["reasoning_effort"],
                Value::Null
            );
        },
    );
}

/// Partially priced work keeps the complete-cohort cost unavailable while
/// reporting measured, unpriced and missing-run coverage over the counted
/// attempts behind it; a valid zero cost stays zero and neither coverage
/// nor pricing changes scores or ranks.
#[test]
fn implementers_report_cost_coverage_without_averaging_priced_attempts() {
    isolated(
        "agents::implementers_report_cost_coverage_without_averaging_priced_attempts",
        || {
            use orbit_core::InvocationInsertParams;
            use orbit_core::application::agents::{AgentQuery, agent_detail, agent_scorecard};
            use orbit_types::telemetry::InvocationTrace;
            let fixture = Fixture::new();
            let now = Utc::now();
            let priced_cost = |fixture: &Fixture, run: &str, cost: f64, task: &str| {
                fixture
                    .runtime
                    .insert_invocation_trace_record(&InvocationInsertParams {
                        agent_execution_id: None,
                        job_run_id: run.into(),
                        account: None,
                        activity_id: "implement_one".into(),
                        agent: "codex".into(),
                        provider: None,
                        model: None,
                        task_ids: vec![task.into()],
                        trace: InvocationTrace {
                            provider_cost_usd: Some(cost),
                            ..Default::default()
                        },
                    })
                    .unwrap();
            };
            let landed_task = |fixture: &Fixture, title: &str| {
                add(
                    fixture,
                    Owned {
                        title,
                        orchestrator: None,
                        origin: None,
                        complexity: TaskComplexity::Medium,
                        status: TaskStatus::Done,
                        relations: vec![],
                    },
                )
            };
            // One priced and one unpriced local run for `coverage`, both
            // with no recorded model so they share one row.
            for (run, priced) in [
                ("jrun-coverage-priced", true),
                ("jrun-coverage-unpriced", false),
            ] {
                let id = landed_task(&fixture, run);
                let start = now - Duration::hours(3);
                implementation_run(
                    &fixture,
                    run,
                    &id,
                    "coverage",
                    None,
                    start,
                    now - Duration::hours(2),
                );
                append(
                    &fixture,
                    &id,
                    start,
                    "started",
                    None,
                    Some(TaskStatus::InProgress),
                );
                append(
                    &fixture,
                    &id,
                    now - Duration::hours(2),
                    "status_changed",
                    None,
                    Some(TaskStatus::Done),
                );
                if priced {
                    priced_cost(&fixture, run, 6.0, &id);
                }
            }
            // One attempt pulled by another machine with no run recorded.
            let missing = landed_task(&fixture, "missing run");
            append(
                &fixture,
                &missing,
                now - Duration::hours(4),
                "crew_assigned",
                Some("assigned crew `coverage` from pool:medium"),
                None,
            );
            append(
                &fixture,
                &missing,
                now - Duration::hours(3),
                "pulled_by",
                Some(&json!({"machine_id": "hm_fixture"}).to_string()),
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &missing,
                now - Duration::hours(2),
                "status_changed",
                None,
                Some(TaskStatus::Done),
            );
            // A separate row whose only run cost a valid zero.
            let zero = landed_task(&fixture, "zero cost");
            let start = now - Duration::hours(3);
            implementation_run(
                &fixture,
                "jrun-zero",
                &zero,
                "zero",
                None,
                start,
                now - Duration::hours(2),
            );
            append(
                &fixture,
                &zero,
                start,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &zero,
                now - Duration::hours(2),
                "status_changed",
                None,
                Some(TaskStatus::Done),
            );
            priced_cost(&fixture, "jrun-zero", 0.0, &zero);

            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let facts = fixture.runtime.agent_scan().unwrap();
            let card = agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            let stat = |row: &Value, key: &str| {
                row["stats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|stat| stat["key"] == key)
                    .unwrap()["value"]
                    .clone()
            };
            let coverage = card["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["crew"] == "coverage")
                .unwrap()
                .clone();
            assert_eq!(
                stat(&coverage, "cost_per_weighted_landed"),
                Value::Null,
                "one missing run suppresses the complete-cohort average, never a priced-only average"
            );
            assert_eq!(
                stat(&coverage, "cost_coverage"),
                json!("1 measured · 1 unpriced · 1 missing run of 3 counted attempts")
            );
            assert_eq!(
                stat(&coverage, "cost_coverage_attempts"),
                json!(3),
                "the named denominator is the counted attempts behind the cost calculation"
            );
            assert_eq!(stat(&coverage, "cost_measured_attempts"), json!(1));
            assert_eq!(stat(&coverage, "cost_unpriced_attempts"), json!(1));
            assert_eq!(stat(&coverage, "cost_missing_run_attempts"), json!(1));
            let zero_row = card["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["crew"] == "zero")
                .unwrap();
            assert_eq!(
                stat(zero_row, "cost_per_weighted_landed"),
                json!(0.0),
                "valid zero spend stays zero"
            );
            assert_eq!(stat(zero_row, "cost_measured_attempts"), json!(1));
            let (score, rank) = (coverage["score"].clone(), coverage["rank"].clone());

            let server = fixture.server(false);
            let http_card = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&min_support=1"
            )));
            assert_eq!(
                http_card["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["crew"] == "coverage")
                    .unwrap(),
                &coverage
            );
            let detail = agent_detail(
                &facts,
                AgentRole::Implementers,
                &query,
                "coverage",
                Some("model unknown"),
                std::time::Duration::ZERO,
            )
            .unwrap();
            assert_eq!(detail["row"], coverage);
            let http_detail = json_ok(server.get(&format!("/api/agents/implementers/detail?{WS}&crew=coverage&model=model%20unknown&min_support=1")));
            assert_eq!(http_detail["row"], coverage);

            // Pricing the unpriced run updates coverage only: the missing
            // run still suppresses the average and the score is unchanged.
            priced_cost(&fixture, "jrun-coverage-unpriced", 12.0, &missing);
            let next = agent_scorecard(
                &fixture.runtime.agent_scan().unwrap(),
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            let next_coverage = next["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["crew"] == "coverage")
                .unwrap();
            assert_eq!(stat(next_coverage, "cost_per_weighted_landed"), Value::Null);
            assert_eq!(stat(next_coverage, "cost_measured_attempts"), json!(2));
            assert_eq!(stat(next_coverage, "cost_unpriced_attempts"), json!(0));
            assert_eq!(
                (
                    next_coverage["score"].clone(),
                    next_coverage["rank"].clone()
                ),
                (score, rank),
                "cost coverage never changes score or rank"
            );
        },
    );
}

/// A task worked in an interactive session (moved by hand, no job run) has no
/// cost or run time. Before the execution split it joined its crew's row as a
/// missing run, suppressing that row's cost and inflating its units: the
/// implementer counts default to managed tasks, `execution` adds or isolates
/// interactive ones, cost always counts managed tasks only, and the header
/// shows each row's interactive and unclassified tasks whatever the scope.
#[test]
fn implementer_counts_default_to_managed_tasks_and_show_interactive_work_per_crew() {
    isolated(
        "agents::implementer_counts_default_to_managed_tasks_and_show_interactive_work_per_crew",
        || {
            use orbit_core::InvocationInsertParams;
            use orbit_core::application::agents::{AgentQuery, agent_scorecard};
            use orbit_types::telemetry::InvocationTrace;
            let fixture = Fixture::new();
            let now = Utc::now();
            let task = |fixture: &Fixture, title: &str, status: TaskStatus| {
                add(
                    fixture,
                    Owned {
                        title,
                        orchestrator: None,
                        origin: None,
                        complexity: TaskComplexity::Medium,
                        status,
                        relations: vec![],
                    },
                )
            };
            // Managed: a priced job run worked and landed it.
            let managed = task(&fixture, "managed", TaskStatus::Done);
            let start = now - Duration::hours(5);
            implementation_run(
                &fixture,
                "jrun-split-managed",
                &managed,
                "split",
                None,
                start,
                now - Duration::hours(4),
            );
            append(
                &fixture,
                &managed,
                start,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &managed,
                now - Duration::hours(4),
                "status_changed",
                None,
                Some(TaskStatus::Done),
            );
            fixture
                .runtime
                .insert_invocation_trace_record(&InvocationInsertParams {
                    agent_execution_id: None,
                    job_run_id: "jrun-split-managed".into(),
                    account: None,
                    activity_id: "implement_one".into(),
                    agent: "codex".into(),
                    provider: None,
                    model: None,
                    task_ids: vec![managed.clone()],
                    trace: InvocationTrace {
                        provider_cost_usd: Some(4.0),
                        ..Default::default()
                    },
                })
                .unwrap();
            // Interactive: the same crew, moved through review to done by
            // hand with no job run; one regression names it.
            let interactive = task(&fixture, "interactive", TaskStatus::Done);
            append(
                &fixture,
                &interactive,
                now - Duration::hours(4),
                "crew_assigned",
                Some("assigned crew `split` by hand"),
                None,
            );
            for (hours, event, status) in [
                (3, "started", TaskStatus::InProgress),
                (2, "status_changed", TaskStatus::Review),
                (1, "status_changed", TaskStatus::Done),
            ] {
                append(
                    &fixture,
                    &interactive,
                    now - Duration::hours(hours),
                    event,
                    None,
                    Some(status),
                );
            }
            regression(
                &fixture,
                &interactive,
                RegressionNature::DefectInNew,
                RegressionEscape::NoTest,
            );
            // Unclassified: started by hand and not yet in review.
            let open = task(&fixture, "unclassified", TaskStatus::InProgress);
            append(
                &fixture,
                &open,
                now - Duration::hours(4),
                "crew_assigned",
                Some("assigned crew `split` by hand"),
                None,
            );
            append(
                &fixture,
                &open,
                now - Duration::hours(1),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );

            let facts = fixture.runtime.agent_scan().unwrap();
            let row = |execution: Option<&str>| {
                let query = AgentQuery::parse(Some("30d"), Some(1))
                    .unwrap()
                    .with_execution(execution)
                    .unwrap();
                let card = agent_scorecard(
                    &facts,
                    AgentRole::Implementers,
                    &query,
                    std::time::Duration::ZERO,
                );
                let row = card["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["crew"] == "split")
                    .cloned();
                (card, row)
            };
            let stat = |row: &Value, key: &str| {
                row["stats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|stat| stat["key"] == key)
                    .unwrap()["value"]
                    .clone()
            };

            let (card, managed_row) = row(None);
            assert_eq!(card["execution"], "managed", "managed is the default");
            let managed_row = managed_row.expect("the managed task's row");
            assert_eq!(managed_row["units"], 1, "only the managed task counts");
            assert_eq!(stat(&managed_row, "cost_per_weighted_landed"), json!(4.0));
            assert_eq!(stat(&managed_row, "cost_missing_run_attempts"), json!(0));
            assert_eq!(
                card["execution_split"],
                json!([{
                    "crew": "split",
                    "model": "model unknown",
                    "managed_tasks": 1,
                    "interactive_tasks": 1,
                    "unclassified_tasks": 1,
                    "interactive_landed": 1,
                    "interactive_regressed": 1,
                }]),
                "interactive work stays visible per crew under the managed default"
            );

            let (_, all_row) = row(Some("all"));
            let all_row = all_row.expect("the combined row");
            assert_eq!(
                all_row["units"], 2,
                "managed and interactive, never unclassified"
            );
            assert_eq!(
                stat(&all_row, "cost_per_weighted_landed"),
                json!(4.0),
                "cost counts managed tasks only whatever the scope"
            );
            assert_eq!(stat(&all_row, "cost_coverage_attempts"), json!(1));

            let (_, interactive_row) = row(Some("interactive"));
            let interactive_row = interactive_row.expect("the interactive series");
            assert_eq!(interactive_row["units"], 1);
            assert_eq!(stat(&interactive_row, "cost_coverage_attempts"), json!(0));
            assert_eq!(stat(&interactive_row, "median_run_seconds"), Value::Null);

            let server = fixture.server(false);
            let http = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&min_support=1&execution=all"
            )));
            assert_eq!(http["execution"], "all");
            assert_eq!(
                http["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["crew"] == "split")
                    .unwrap()["units"],
                2
            );
            let refused = server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&execution=offline"
            ));
            assert_eq!(
                refused.status().as_u16(),
                400,
                "an unknown scope is refused"
            );
        },
    );
}

/// One fixture workspace, read through the runtime API and the routes: every
/// problem has exactly one owner or a Not scored reason and the attribution
/// totals are the problems behind the four roles; a task two crews worked
/// charges each crew only for its own attempt and credits the final run; a
/// pulled attempt with no local run charges the crew its history assigned;
/// the orchestrator ranking keeps an incomplete, untested regression off the
/// orchestrator and the implementer score while retaining its implementer
/// ownership; and every selection after the first scan is served from the cached scan.
#[test]
fn agent_scorecards_attribute_each_problem_once_and_serve_selections_from_one_scan() {
    isolated(
        "agents::agent_scorecards_attribute_each_problem_once_and_serve_selections_from_one_scan",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let hours = Duration::hours;
            let minutes = Duration::minutes;

            // opus: a landed initiative two crews implemented. luna's run
            // failed validation and was requeued by hand; sol's run landed.
            // The reviewer astra rejected sol's candidate, waiving one
            // finding, then accepted it.
            let shared = add(
                &fixture,
                Owned {
                    title: "two crews",
                    orchestrator: Some("opus"),
                    origin: Some(TaskOrigin::OrchestratorInitiative),
                    complexity: TaskComplexity::Medium,
                    status: TaskStatus::Done,
                    relations: vec![],
                },
            );
            let luna_start = now - hours(5);
            let sol_start = now - hours(3);
            implementation_run(
                &fixture,
                "jrun-luna",
                &shared,
                "luna",
                None,
                luna_start - Duration::seconds(1),
                now - hours(4),
            );
            implementation_run(
                &fixture,
                "jrun-sol",
                &shared,
                "sol",
                Some("gpt-sol"),
                sol_start - Duration::seconds(1),
                now - hours(2),
            );
            append(
                &fixture,
                &shared,
                luna_start,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &shared,
                now - hours(4),
                "workflow_run_failed",
                Some("validation failed: 3 assertions"),
                Some(TaskStatus::Blocked),
            );
            append(
                &fixture,
                &shared,
                now - hours(4) + minutes(10),
                "operator_requeued",
                None,
                Some(TaskStatus::Backlog),
            );
            append(
                &fixture,
                &shared,
                sol_start,
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &shared,
                now - hours(2),
                "status_changed",
                Some("delivered by pull request #12 merged as abc1234"),
                Some(TaskStatus::Done),
            );
            comment(
                &fixture,
                &shared,
                "before-PR review settled attempt `rv-1`: verdict **reject** (2 findings)\n\
                 - Reviewer: crew `astra` (codex / gpt-astra)\n\
                 - `F1` [high, open] the handler drops errors\n\
                 - `F2` [low, disposed: accepted risk] a log line is noisy",
            );
            comment(
                &fixture,
                &shared,
                "before-landing review settled attempt `rv-2`: verdict **accept**\n\
                 - Reviewer: crew `astra` (codex / gpt-astra)",
            );

            // opus: a landed task whose description was edited mid-run.
            let edited = add(
                &fixture,
                Owned {
                    title: "edited mid-run",
                    orchestrator: Some("opus"),
                    origin: Some(TaskOrigin::OperatorRequest),
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Done,
                    relations: vec![],
                },
            );
            implementation_run(
                &fixture,
                "jrun-sol-2",
                &edited,
                "sol",
                Some("gpt-sol"),
                now - hours(2) - Duration::seconds(1),
                now - hours(1),
            );
            append(
                &fixture,
                &edited,
                now - hours(2),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &edited,
                now - minutes(90),
                "updated",
                Some(&task_fields_updated_note(&[DESCRIPTION_DOCUMENT])),
                None,
            );
            append(
                &fixture,
                &edited,
                now - hours(1),
                "status_changed",
                Some("delivered by pull request #13 merged as def5678"),
                Some(TaskStatus::Done),
            );

            // opus: a run interrupted by the host, then requeued.
            let interrupted = add(
                &fixture,
                Owned {
                    title: "interrupted",
                    orchestrator: Some("opus"),
                    origin: Some(TaskOrigin::OperatorRequest),
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Backlog,
                    relations: vec![],
                },
            );
            append(
                &fixture,
                &interrupted,
                now - hours(6),
                "started",
                None,
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &interrupted,
                now - hours(6) + minutes(10),
                "workflow_run_interrupted",
                None,
                Some(TaskStatus::Blocked),
            );
            append(
                &fixture,
                &interrupted,
                now - hours(6) + minutes(20),
                "operator_requeued",
                None,
                Some(TaskStatus::Backlog),
            );

            // opus: another machine pulled this task, so no local job run
            // records its attempt; history assigned it crew nova.
            let pulled = add(
                &fixture,
                Owned {
                    title: "pulled by another machine",
                    orchestrator: Some("opus"),
                    origin: None,
                    complexity: TaskComplexity::Low,
                    status: TaskStatus::Backlog,
                    relations: vec![],
                },
            );
            append(
                &fixture,
                &pulled,
                now - hours(8),
                "crew_assigned",
                Some("assigned crew `nova` from pool:low"),
                None,
            );
            append(
                &fixture,
                &pulled,
                now - hours(7),
                "pulled_by",
                Some(
                    &json!({
                        "machine_id": "hm_fixture",
                        "run_context": {
                            "run_id": "jrun-remote",
                            "job_name": "workspace_pull_pipeline",
                            "machine_name": "remote-mac",
                        },
                    })
                    .to_string(),
                ),
                Some(TaskStatus::InProgress),
            );
            append(
                &fixture,
                &pulled,
                now - hours(7) + minutes(30),
                "workflow_run_failed",
                Some("validation failed: build broke"),
                Some(TaskStatus::Blocked),
            );
            append(
                &fixture,
                &pulled,
                now - hours(7) + minutes(40),
                "operator_requeued",
                None,
                Some(TaskStatus::Backlog),
            );

            // sonnet: one rejected initiative, below the minimum support.
            add(
                &fixture,
                Owned {
                    title: "dropped initiative",
                    orchestrator: Some("sonnet"),
                    origin: Some(TaskOrigin::OrchestratorInitiative),
                    complexity: TaskComplexity::Hard,
                    status: TaskStatus::Rejected,
                    relations: vec![],
                },
            );

            // Regressions, each owned by the safeguard its escape names.
            regression(
                &fixture,
                &edited,
                RegressionNature::BrokeExisting,
                RegressionEscape::SpecAmbiguous,
            );
            regression(
                &fixture,
                &shared,
                RegressionNature::DefectInNew,
                RegressionEscape::ReviewMissed,
            );
            regression(
                &fixture,
                &edited,
                RegressionNature::BrokeExisting,
                RegressionEscape::Nondeterministic,
            );

            let query =
                orbit_core::application::agents::AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let before_no_test = fixture.runtime.agent_scan().unwrap();
            let implemented_before = orbit_core::application::agents::agent_scorecard(
                &before_no_test,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            let strip_before = orbit_core::application::agents::agent_attribution(
                &before_no_test,
                &query,
                std::time::Duration::ZERO,
            );
            rated_regression(
                &fixture,
                &shared,
                RegressionNature::Incomplete,
                RegressionEscape::NoTest,
                DefectSeverity::High,
            );

            // The runtime API: one owner per problem.
            let facts = fixture.runtime.agent_scan().unwrap();
            let owner_of = |task: &str, problem: Problem| -> Vec<(Owner, Option<String>)> {
                facts
                    .charges()
                    .iter()
                    .filter(|charge| charge.task_id == task && charge.problem == problem)
                    .map(|charge| (charge.owner, charge.crew.clone()))
                    .collect()
            };
            let implementer =
                |crew: &str| (Owner::Role(AgentRole::Implementers), Some(crew.into()));
            assert_eq!(
                owner_of(&shared, Problem::RepeatRun),
                vec![implementer("luna")],
                "the rerun followed luna's failed run"
            );
            assert_eq!(
                owner_of(&shared, Problem::OperatorRequeue),
                vec![implementer("luna")]
            );
            assert_eq!(
                owner_of(&shared, Problem::ReviewReject),
                vec![implementer("sol")],
                "the reject settled sol's candidate"
            );
            assert_eq!(
                owner_of(&shared, Problem::DisposedFinding),
                vec![(Owner::Role(AgentRole::Reviewers), Some("astra".into()))],
                "only the disposed finding counts against the reviewer"
            );
            assert_eq!(
                owner_of(&edited, Problem::SpecEdit),
                vec![(Owner::Role(AgentRole::Orchestrators), Some("opus".into()))]
            );
            assert_eq!(
                owner_of(&interrupted, Problem::OperatorRequeue),
                vec![(Owner::NotScored("infrastructure"), None)]
            );
            assert_eq!(
                owner_of(&pulled, Problem::OperatorRequeue),
                vec![implementer("nova")],
                "a pulled attempt charges the crew history assigned it"
            );
            let mut regressions: Vec<_> = facts
                .charges()
                .iter()
                .filter(|charge| charge.problem == Problem::Regression)
                .map(|charge| (charge.task_id.clone(), charge.owner, charge.crew.clone()))
                .collect();
            regressions.sort();
            let mut expected = vec![
                (
                    shared.clone(),
                    Owner::Role(AgentRole::Implementers),
                    Some("sol".into()),
                ),
                (
                    shared.clone(),
                    Owner::Role(AgentRole::Reviewers),
                    Some("astra".into()),
                ),
                (
                    edited.clone(),
                    Owner::Role(AgentRole::Orchestrators),
                    Some("opus".into()),
                ),
                (edited.clone(), Owner::NotScored("nondeterministic"), None),
            ];
            expected.sort();
            assert_eq!(
                regressions, expected,
                "an incomplete, untested regression is the final implementer's; only \
                 spec_ambiguous reaches the orchestrator"
            );

            // Each crew is charged only inside its own attempt, and the
            // landed credit goes to the final run.
            for charge in facts.charges().iter().filter(|charge| {
                charge.task_id == shared && charge.owner == Owner::Role(AgentRole::Implementers)
            }) {
                let (run, start) = match charge.crew.as_deref() {
                    Some("luna") => ("jrun-luna", luna_start),
                    Some("sol") => ("jrun-sol", sol_start),
                    other => panic!("only luna and sol worked the task: {other:?}"),
                };
                assert_eq!(charge.run_id.as_deref(), Some(run), "{charge:?}");
                assert_eq!(charge.anchor, start, "{charge:?}");
            }
            let pulled_charge = facts
                .charges()
                .iter()
                .find(|charge| charge.task_id == pulled)
                .unwrap();
            assert_eq!(pulled_charge.run_id.as_deref(), Some("jrun-remote"));
            assert_eq!(pulled_charge.model, None, "no run recorded the model");
            let credit = facts
                .credits()
                .iter()
                .find(|credit| credit.task_id == shared)
                .expect("the landed task is credited");
            assert_eq!(
                (
                    credit.crew.as_deref(),
                    credit.model.as_deref(),
                    credit.run_id.as_deref()
                ),
                (Some("sol"), Some("gpt-sol"), Some("jrun-sol"))
            );
            assert_eq!(
                facts
                    .credits()
                    .iter()
                    .filter(|credit| credit.task_id == shared)
                    .count(),
                1,
                "only the final run is credited"
            );

            let implemented = orbit_core::application::agents::agent_scorecard(
                &facts,
                AgentRole::Implementers,
                &query,
                std::time::Duration::ZERO,
            );
            assert_eq!(implemented["status"], "ok");
            assert!(
                implemented["terms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|term| term["key"] != "escaped_regressions"),
                "escaped regressions remain unscored context"
            );
            assert_eq!(
                implemented["fleet_average"],
                implemented_before["fleet_average"]
            );
            for row in implemented["rows"].as_array().unwrap() {
                let before = implemented_before["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|before| before["crew"] == row["crew"] && before["model"] == row["model"])
                    .unwrap();
                for field in ["counts", "expected", "raw", "score", "rank"] {
                    assert_eq!(
                        row[field], before[field],
                        "a no_test regression changes ownership context, not {field}: {row}"
                    );
                }
            }
            let sol = implemented["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["crew"] == "sol")
                .unwrap();
            assert_eq!(sol["units"], 2);
            assert_eq!(
                sol["stats"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|stat| stat["key"] == "escaped_regressions")
                    .unwrap()["value"],
                4.0,
                "only no_test reaches the implementer, weighted by its high severity; review_missed is the reviewer's"
            );
            assert_eq!(sol["counts"]["review_rejects"], 1);
            assert_eq!(
                sol["counts"]["first_pass_accepts"], 0,
                "a later accept does not erase the first reject"
            );
            assert_eq!(
                sol["counts"]["crew_rework"], 0,
                "the review reject is not also crew rework"
            );
            let luna = implemented["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["crew"] == "luna")
                .unwrap();
            assert_eq!(
                luna["counts"]["weighted_landed"], 0,
                "landed credit goes only to the final implementer"
            );
            assert_eq!(luna["counts"]["crew_rework"], 2);
            let cost = sol["stats"]
                .as_array()
                .unwrap()
                .iter()
                .find(|stat| stat["key"] == "cost_per_weighted_landed")
                .unwrap();
            assert!(
                cost["value"].is_null(),
                "missing telemetry is not free work"
            );

            // The routes: the first request scans, every later selection is
            // served from that scan.
            let server = fixture.counted_task_server();
            let before = server.task_query_trace().len();
            let scorecard = json_ok(server.get(&format!(
                "/api/agents/orchestrators/scorecard?{WS}&window=30d&min_support=3"
            )));
            let cold = server.task_query_trace()[before..].to_vec();
            assert!(
                operations(&cold, "task_history_read") > 0,
                "the first request scans: {cold:?}"
            );
            let warm_from = server.task_query_trace().len();

            assert_eq!(scorecard["status"], "ok");
            assert_eq!(scorecard["window"], "30d");
            let terms: Vec<&str> = scorecard["terms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|term| term["key"].as_str().unwrap())
                .collect();
            let rows = scorecard["rows"].as_array().unwrap();
            assert_eq!(rows.len(), 2, "one row per orchestrator: {scorecard}");
            let (opus, sonnet) = (&rows[0], &rows[1]);
            assert_eq!(
                (&opus["crew"], &opus["model"], &opus["units"], &opus["rank"]),
                (&json!("opus"), &json!("opus"), &json!(4), &json!(1)),
                "an orchestrator row carries the model its crew was configured with when the tasks were created: {opus}"
            );
            assert_eq!(
                (&sonnet["crew"], &sonnet["below_support"], &sonnet["rank"]),
                (&json!("sonnet"), &json!(true), &Value::Null),
                "a row below the minimum support comes last, unranked: {sonnet}"
            );
            assert_eq!(opus["counts"]["spec_defects"], 2, "{opus}");
            assert_eq!(opus["counts"]["spec_rework"], 0, "{opus}");
            let fleet = as_f64(&scorecard["fleet_average"]);
            for row in rows {
                let units = as_f64(&row["units"]);
                for term in &terms {
                    let rate = as_f64(&row["counts"][term]) * 100.0 / units;
                    assert!(
                        (as_f64(&row["per_100"][term]) - rate).abs() < 0.01,
                        "{term} is a rate per 100 owned tasks: {row}"
                    );
                }
                let (raw, score) = (as_f64(&row["raw"]), as_f64(&row["score"]));
                assert!(
                    score <= raw && score >= raw.min(fleet),
                    "the score is the raw score, pulled down toward the fleet average at most: {row}"
                );
            }

            let detail = json_ok(server.get(&format!(
                "/api/agents/orchestrators/detail?{WS}&window=30d&min_support=3&crew=opus"
            )));
            assert_eq!(detail["row"], *opus);
            let spec_defects = detail["terms"]
                .as_array()
                .unwrap()
                .iter()
                .find(|term| term["key"] == "spec_defects")
                .unwrap();
            let tasks: Vec<&str> = spec_defects["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|task| task["task_id"].as_str().unwrap())
                .collect();
            assert_eq!(
                tasks,
                vec![edited.as_str()],
                "the mid-run edit and the spec_ambiguous regression; not the untested one"
            );

            // Attribution retains unscored implementer context, independently of scores.
            let strip = json_ok(server.get(&format!("/api/agents/attribution?{WS}&window=30d")));
            let owners = &strip["owners"];
            assert_eq!(
                owners,
                &json!({
                    "orchestrators": 2,
                    "implementers": 5,
                    "reviewers": 2,
                    "pilots": 0,
                    "not_scored": {
                        "count": 2,
                        "reasons": {"infrastructure": 1, "nondeterministic": 1},
                    },
                }),
                "{strip}"
            );
            assert_eq!(strip["total"], 11);
            assert_eq!(strip_before["owners"]["implementers"], 4);
            assert_eq!(
                owners["implementers"],
                strip_before["owners"]["implementers"].as_u64().unwrap() + 1,
                "the no_test escape adds one Implementers-owned problem, regardless of severity"
            );
            let implemented_http = json_ok(server.get(&format!(
                "/api/agents/implementers/scorecard?{WS}&window=30d&min_support=1"
            )));
            assert_eq!(implemented_http["rows"], implemented["rows"]);
            assert_eq!(implemented_http["terms"], implemented["terms"]);
            for role in ["orchestrators", "implementers", "reviewers", "pilots"] {
                let charged: u64 = strip["charged"][role]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["problems"].as_u64().unwrap())
                    .sum();
                assert_eq!(owners[role], charged, "{role} rows sum to its total");
            }
            let ranked: f64 = rows
                .iter()
                .map(|row| {
                    as_f64(&row["counts"]["spec_defects"]) + as_f64(&row["counts"]["spec_rework"])
                })
                .sum();
            assert_eq!(
                ranked,
                as_f64(&owners["orchestrators"]),
                "the orchestrator ranking counts every orchestrator charge"
            );
            assert!(
                strip["charged"]["implementers"]
                    .as_array()
                    .unwrap()
                    .contains(&json!({"crew": "luna", "model": "model unknown", "problems": 2})),
                "a run without a model is `model unknown`: {strip}"
            );

            let payload = json_ok(server.get(&format!(
                "/api/agents/pilots/scorecard?{WS}&window=all&min_support=1"
            )));
            assert_eq!(payload["status"], "ok", "{payload}");
            assert_eq!(payload["rows"], json!([]), "{payload}");
            assert!(
                !payload["terms"].as_array().unwrap().is_empty(),
                "{payload}"
            );
            assert_eq!(
                (&payload["window"], &payload["requested_window"]),
                (&json!("90d"), &json!("all"))
            );
            json_ok(server.get(&format!(
                "/api/agents/orchestrators/scorecard?{WS}&window=7d&min_support=1"
            )));
            let warm = server.task_query_trace()[warm_from..].to_vec();
            assert_eq!(
                operations(&warm, "task_history_read"),
                0,
                "role, window, minimum support and detail changes reuse the scan: {warm:?}"
            );

            for refused in [
                format!("/api/agents/coders/scorecard?{WS}"),
                format!("/api/agents/orchestrators/scorecard?{WS}&window=1y"),
                format!("/api/agents/orchestrators/scorecard?{WS}&min_support=0"),
                format!("/api/agents/orchestrators/detail?{WS}"),
                format!("/api/agents/orchestrators/detail?{WS}&crew=nobody"),
                format!("/api/agents/implementers/detail?{WS}&crew=nobody"),
                format!("/api/agents/attribution?{WS}&window=1y"),
            ] {
                assert_eq!(server.get(&refused).status().as_u16(), 400, "{refused}");
            }
        },
    );
}

#[test]
fn count_windows_exclude_old_attempts_on_recently_commented_tasks() {
    isolated(
        "agents::count_windows_exclude_old_attempts_on_recently_commented_tasks",
        || {
            for with_recent in [false, true] {
                let fixture = Fixture::new();
                let now = Utc::now();
                let old_start = now - Duration::days(120);
                let old = add(
                    &fixture,
                    Owned {
                        title: "old work",
                        orchestrator: None,
                        origin: None,
                        complexity: TaskComplexity::Low,
                        status: TaskStatus::Done,
                        relations: vec![],
                    },
                );
                let path = bundle_dir(&fixture.path(""), &old)
                    .unwrap()
                    .join(orbit_types::task::TASK_ENVELOPE_FILE_NAME);
                let mut envelope: orbit_types::task::TaskEnvelopeV2 =
                    serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                envelope.created_at = old_start - Duration::days(1);
                envelope.updated_at = old_start;
                fs::write(&path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
                append(
                    &fixture,
                    &old,
                    old_start,
                    "started",
                    None,
                    Some(TaskStatus::InProgress),
                );
                append(
                    &fixture,
                    &old,
                    old_start + Duration::hours(1),
                    "status_changed",
                    None,
                    Some(TaskStatus::Done),
                );
                comment(&fixture, &old, "recent comment");
                let recent_start = now - Duration::days(2);
                if with_recent {
                    let recent = add(
                        &fixture,
                        Owned {
                            title: "recent work",
                            orchestrator: None,
                            origin: None,
                            complexity: TaskComplexity::Low,
                            status: TaskStatus::Done,
                            relations: vec![],
                        },
                    );
                    implementation_run(
                        &fixture,
                        "jrun-recent",
                        &recent,
                        "recent",
                        Some("model"),
                        recent_start - Duration::seconds(1),
                        recent_start + Duration::hours(1),
                    );
                    append(
                        &fixture,
                        &recent,
                        recent_start,
                        "started",
                        None,
                        Some(TaskStatus::InProgress),
                    );
                    append(
                        &fixture,
                        &recent,
                        recent_start + Duration::hours(1),
                        "status_changed",
                        None,
                        Some(TaskStatus::Done),
                    );
                }
                let server = fixture.server(false);
                for (window, size) in [
                    ("last50", 50),
                    ("last200", 200),
                    ("last500", 500),
                    ("last1000", 1000),
                    ("90d", 0),
                ] {
                    let response = json_ok(server.get(&format!(
                        "/api/agents/implementers/scorecard?{WS}&window={window}&min_support=1"
                    )));
                    let since = DateTime::parse_from_rfc3339(response["since"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc);
                    if size > 0 {
                        assert_eq!(response["window_attempts"], size);
                        let expected = if with_recent {
                            recent_start
                        } else {
                            DateTime::parse_from_rfc3339(response["computed_at"].as_str().unwrap())
                                .unwrap()
                                .with_timezone(&Utc)
                                - Duration::days(90)
                        };
                        assert_eq!(
                            since.timestamp(),
                            expected.timestamp(),
                            "{window}: {response}"
                        );
                    } else {
                        assert!(response["window_attempts"].is_null());
                    }
                    let rows = response["rows"].as_array().unwrap();
                    assert_eq!(rows.len(), usize::from(with_recent), "{window}: {response}");
                    if with_recent {
                        assert_eq!(rows[0]["crew"], "recent");
                        assert_eq!(rows[0]["units"], 1);
                    }
                }
            }
        },
    );
}
