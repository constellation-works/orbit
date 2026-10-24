//! The pilots scorecard (`/api/agents/pilots/*`) through the runtime API and
//! the HTTP API, over fixture tasks with task-pilot receipts, landed commits,
//! reservations, lock denials and priced implementation runs.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::agents::{AgentQuery, AgentRole, agent_detail, agent_scorecard};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{InvocationInsertParams, OrbitError, TaskStatus};
use orbit_types::task::TaskComplexity;
use orbit_types::telemetry::InvocationTrace;
use serde_json::{Value, json};

use super::agents::implementation_run;
use super::codebase::{
    add, add_with_status, append_event, init_repo, land, pilot_receipt, pilot_run, reserve,
    reserve_request, seed_pilot_run,
};
use super::support::{Fixture, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

fn row<'a>(card: &'a Value, crew: &str, model: &str) -> &'a Value {
    card["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["crew"] == crew && row["model"] == model)
        .unwrap_or_else(|| panic!("a {crew} / {model} row: {card}"))
}

fn stat(row: &Value, key: &str) -> Value {
    row["stats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|stat| stat["key"] == key)
        .unwrap_or_else(|| panic!("{key}: {row}"))["value"]
        .clone()
}

fn term<'a>(detail: &'a Value, key: &str) -> &'a Value {
    detail["terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|term| term["key"] == key)
        .unwrap_or_else(|| panic!("{key}: {detail}"))
}

fn scorecard(fixture: &Fixture, window: &str) -> Value {
    let facts = fixture.runtime.agent_scan().unwrap();
    let query = AgentQuery::parse(Some(window), Some(1)).unwrap();
    agent_scorecard(&facts, AgentRole::Pilots, &query, StdDuration::ZERO)
}

/// A refused lock request of `task` on `file` at `at`, held by a task or a
/// reservation, as the retained denial row the scan reads.
fn denial(
    fixture: &Fixture,
    id: &str,
    task: &str,
    file: &str,
    held_by: (&str, &str),
    at: DateTime<Utc>,
) {
    let arguments = json!({
        "task_ids": [task],
        "files": [format!("file:{file}")],
        "conflicts": [{"file": format!("file:{file}"), "held_by": held_by.0, "held_by_id": held_by.1}],
        "owner_run_id": null,
    });
    let row = [id.to_string(), at.to_rfc3339(), arguments.to_string()];
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "INSERT INTO task_lock_denials(execution_id, workspace_id, timestamp,
                       job_run_id, arguments_json)
                     VALUES (?1, 'ws_http_fixture', ?2, NULL, ?3)",
                    row,
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

/// A pilot's task crew is not the pilot: use the frozen activity draw and
/// invocation, including legacy and missing evidence, at the HTTP boundary.
#[test]
fn pilots_attribute_assessments_to_recorded_activity() {
    isolated(
        "agents_pilots::pilots_attribute_assessments_to_recorded_activity",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            let purpose = "workflow.purpose_crews.task_pilot";
            let legacy = "workflow.low_complexity_crews";
            for (index, key, crew, model) in [
                (0, Some(purpose), "haiku", Some("actual-pilot-model")),
                (1, Some(legacy), "luna", Some("legacy-pilot-model")),
                (2, Some(purpose), "haiku", None),
                (3, None, "ignored", Some("unattributed-pilot-model")),
                (4, Some(purpose), "haiku", Some("actual-pilot-model")),
                (5, Some(purpose), "luna", Some("repair-only-model")),
                (6, Some(purpose), "uninvoked", None),
            ] {
                let run_id = format!("jrun-pilot-attribution-{index}");
                if index < 5 {
                    seed_pilot_run(
                        &fixture,
                        &run_id,
                        "sol",
                        "task-model",
                        now - Duration::minutes(2),
                        now,
                    );
                } else {
                    pilot_run(
                        &fixture,
                        &run_id,
                        "sol",
                        "task-model",
                        now - Duration::minutes(2),
                        now,
                    );
                }
                let mut state = fixture.runtime.read_run_state(&run_id).unwrap().unwrap();
                state.activity_crew_draws.clear();
                if let Some(key) = key {
                    state.activity_crew_draws.insert(
                        key.into(),
                        orbit_types::workflow::ActivityCrewDraw {
                            crew: crew.into(),
                            source: key.into(),
                            eligible_pool: Vec::new(),
                        },
                    );
                }
                if index == 4 {
                    // The purpose draw wins when both keys survived a migration.
                    state.activity_crew_draws.insert(
                        legacy.into(),
                        orbit_types::workflow::ActivityCrewDraw {
                            crew: "wrong-legacy-crew".into(),
                            source: legacy.into(),
                            eligible_pool: Vec::new(),
                        },
                    );
                }
                fixture.runtime.write_run_state(&run_id, &state).unwrap();
                // The latest task-pilot invocation, even with no model, wins over
                // older evidence. Other activities on the run must not supply it.
                for (activity, model) in [
                    (if index == 5 { "repair_pilot" } else { "pilot" }, model),
                    ("prepare_task_pilot", Some("wrong-activity-model")),
                ]
                .into_iter()
                .filter(|_| index != 6)
                {
                    fixture
                        .runtime
                        .insert_invocation_trace_record(&InvocationInsertParams {
                            job_run_id: run_id.clone(),
                            account: None,
                            activity_id: activity.into(),
                            agent: "fixture".into(),
                            provider: None,
                            model: model.map(str::to_string),
                            task_ids: Vec::new(),
                            trace: Default::default(),
                        })
                        .unwrap();
                }
                let task = add_with_status(
                    &fixture,
                    &format!("piloted {index}"),
                    &[],
                    vec![],
                    TaskStatus::Done,
                );
                pilot_receipt(
                    &fixture,
                    &task,
                    now,
                    Some(&run_id),
                    &[],
                    &["file:src/pilot.rs"],
                );
                land(&fixture, &repo, &task, &["src/pilot.rs"], now);
            }
            // Two overlapping runs: only structured apply entries identify which
            // run wrote each legacy receipt. One run also repaired an assessment
            // with a different model, so matching the producing step matters.
            let mut overlap_operations = Vec::new();
            for (index, crew, minutes, cost) in
                [(0, "overlap-a", 10, 2.0), (1, "overlap-b", 20, 6.0)]
            {
                let run = format!("jrun-overlap-{index}");
                pilot_run(
                    &fixture,
                    &run,
                    crew,
                    "task-model",
                    now - Duration::minutes(60),
                    now - Duration::minutes(60 - minutes),
                );
                for (activity, model) in
                    [("pilot", "initial-model"), ("repair_pilot", "repair-model")]
                {
                    if index == 0 && activity == "repair_pilot" {
                        continue;
                    }
                    fixture
                        .runtime
                        .insert_invocation_trace_record(&InvocationInsertParams {
                            job_run_id: run.clone(),
                            account: None,
                            activity_id: activity.into(),
                            agent: "fixture".into(),
                            provider: None,
                            model: Some(model.into()),
                            task_ids: vec![],
                            trace: InvocationTrace {
                                provider_cost_usd: Some(cost),
                                ..Default::default()
                            },
                        })
                        .unwrap();
                }
                let mut state = fixture.runtime.read_run_state(&run).unwrap().unwrap();
                for (step, linked) in [
                    ("apply", false),
                    ("apply_repairs", false),
                    ("apply_repairs", true),
                ] {
                    if index == 0 && step == "apply_repairs" {
                        continue;
                    }
                    let task = add_with_status(
                        &fixture,
                        &format!("{crew} {step} {linked}"),
                        &[],
                        vec![],
                        TaskStatus::Done,
                    );
                    pilot_receipt(
                        &fixture,
                        &task,
                        now - Duration::minutes(55),
                        linked.then_some(run.as_str()),
                        &[],
                        &["file:src/overlap.rs"],
                    );
                    let operation = fixture
                        .runtime
                        .get_task_comments(&task)
                        .unwrap()
                        .last()
                        .unwrap()
                        .receipt()
                        .unwrap()
                        .operation_id
                        .to_string();
                    overlap_operations.push((run.clone(), operation.clone()));
                    if state.pipeline.get(step).is_none() {
                        state.pipeline[step] = json!({"tasks": []});
                    }
                    state.pipeline[step]["tasks"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"operation_id": operation}));
                    // A substring in unrelated output must never select this run.
                    state.step_outputs.insert(
                        99,
                        json!({"diagnostic": format!("prefix-{operation}-suffix")}),
                    );
                    land(&fixture, &repo, &task, &["src/overlap.rs"], now);
                }
                fixture.runtime.write_run_state(&run, &state).unwrap();
            }
            // Another run may mention the operation as part of a longer field.
            // Only exact structured operation ids can break the overlap tie.
            for run in ["jrun-overlap-0", "jrun-overlap-1"] {
                let mut state = fixture.runtime.read_run_state(run).unwrap().unwrap();
                for (_, operation) in overlap_operations.iter().filter(|(owner, _)| owner != run) {
                    state.pipeline["apply"]["tasks"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"operation_id": format!("prefix-{operation}-suffix")}));
                }
                fixture.runtime.write_run_state(run, &state).unwrap();
            }
            let server = fixture.server(false);
            let card = json_ok(server.get(&format!(
                "/api/agents/pilots/scorecard?{WS}&window=7d&min_support=1"
            )));
            let observed: Vec<_> = card["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    (
                        row["crew"].as_str().unwrap(),
                        row["model"].as_str().unwrap(),
                        row["units"].as_u64().unwrap(),
                    )
                })
                .collect();
            let expected = [
                ("haiku", "actual-pilot-model", 2),
                ("overlap-a", "initial-model", 1),
                ("overlap-b", "initial-model", 1),
                ("overlap-b", "repair-model", 2),
            ];
            let missing: Vec<_> = expected
                .iter()
                .filter(|expected| !observed.contains(expected))
                .collect();
            assert!(
                missing.is_empty(),
                "recorded model and overlapping legacy attribution failures: {missing:?}; observed {observed:?}"
            );
            for (crew, model, units, cost, seconds) in [
                ("overlap-a", "initial-model", 1, 2.0, 600),
                ("overlap-b", "initial-model", 1, 12.0, 1200),
                ("overlap-b", "repair-model", 2, 6.0, 1200),
            ] {
                let attributed = row(&card, crew, model);
                assert_eq!(attributed["units"], units);
                assert_eq!(attributed["runs_without_model"], 0);
                assert_eq!(stat(attributed, "cost_per_assessment"), cost);
                assert_eq!(stat(attributed, "median_run_seconds"), seconds);
            }
            assert_eq!(
                row(&card, "haiku", "actual-pilot-model")["units"],
                2,
                "pilot assessments follow the drawn crew, never the piloted task's crew: {card}"
            );
            assert_eq!(row(&card, "luna", "legacy-pilot-model")["units"], 1);
            assert_eq!(row(&card, "haiku", "model unknown")["units"], 1);
            assert_eq!(
                row(&card, "haiku", "model unknown")["runs_without_model"],
                1
            );
            assert_eq!(
                row(&card, "luna", "repair-only-model")["runs_without_model"],
                0
            );
            assert_eq!(
                row(&card, "uninvoked", "model unknown")["runs_without_model"],
                1
            );
            assert_eq!(
                row(&card, "crew unknown", "unattributed-pilot-model")["units"],
                1
            );
            assert_eq!(
                card["rows"].as_array().unwrap().len(),
                9,
                "no task-crew or other-activity rows: {card}"
            );
        },
    );
}

/// Each pilot crew and model is graded against the landed diffs of the tasks
/// it assessed, with the recall, precision and F1 the Codebase ranking gave
/// the same fixture: dir and wildcard selectors cover the files beneath
/// them, a mirrored skill and a CLI golden count through their sources, the
/// filer's selectors are the baseline, a receipt from before the run link is
/// matched to the one pilot run active then, a receipt whose run is gone is
/// `crew unknown`, and no-diff, bulk, unassessed and out-of-window tasks are
/// left out.
#[test]
fn pilots_grade_each_crew_against_landed_diffs() {
    isolated(
        "agents_pilots::pilots_grade_each_crew_against_landed_diffs",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            seed_pilot_run(
                &fixture,
                "jrun-pilot-luna",
                "luna",
                "luna-model",
                now - Duration::minutes(60),
                now - Duration::minutes(50),
            );
            seed_pilot_run(
                &fixture,
                "jrun-pilot-haiku",
                "haiku",
                "claude-haiku-5-5",
                now - Duration::minutes(120),
                now - Duration::minutes(110),
            );
            let piloted = |title: &str,
                           files: &[&str],
                           run: Option<&str>,
                           before: &[&str],
                           after: &[&str],
                           receipt_at: DateTime<Utc>,
                           landed_at: DateTime<Utc>| {
                let task = add_with_status(&fixture, title, &[], vec![], TaskStatus::Done);
                pilot_receipt(&fixture, &task, receipt_at, run, before, after);
                land(&fixture, &repo, &task, files, landed_at);
                task
            };
            let luna = Some("jrun-pilot-luna");
            let haiku = Some("jrun-pilot-haiku");
            let at = now - Duration::minutes(55);
            // luna: every landed file covered; one of five selectors unused.
            piloted(
                "web and skills",
                &[
                    "src/web/app.js",
                    "src/web/style.css",
                    "crates/orbit-core/assets/skills/orbit/SKILL.md",
                    "plugin/skills/orbit/SKILL.md",
                ],
                luna,
                &["file:src/web/app.js"],
                &[
                    "dir:src/web",
                    "file:crates/orbit-core/assets/skills/orbit/SKILL.md",
                    "file:docs/unused.md",
                ],
                at,
                now,
            );
            piloted(
                "cli and goldens",
                &[
                    "src/lib.rs",
                    "src/main.rs",
                    "crates/orbit-cli/src/command/codebase.rs",
                    "crates/orbit-cli/tests/help_goldens/codebase/root.txt",
                ],
                luna,
                &[],
                &[
                    "file:src/*.rs",
                    "file:crates/orbit-cli/src/command/codebase.rs",
                ],
                at,
                now,
            );
            // haiku: half its prediction right where the filer was exact, then
            // a whole directory where the filer named the wrong file.
            let missed = piloted(
                "half right",
                &["src/x.rs", "src/y.rs"],
                haiku,
                &["file:src/x.rs", "file:src/y.rs"],
                &["file:src/x.rs", "file:src/z.rs"],
                now - Duration::minutes(115),
                now,
            );
            piloted(
                "legacy receipt",
                &["docs/guide.md"],
                None,
                &["file:docs/other.md"],
                &["dir:docs"],
                now - Duration::minutes(115),
                now,
            );
            piloted(
                "deleted run",
                &["src/d.rs"],
                Some("jrun-deleted"),
                &[],
                &["file:src/d.rs"],
                at,
                now,
            );
            // Left out: no-diff, bulk, unassessed; too old for 30 days.
            let no_diff = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "no diff".into(),
                    description: "pilot scorecard fixture".into(),
                    tags: vec!["no-diff-expected".into()],
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            pilot_receipt(&fixture, &no_diff, at, luna, &[], &["file:src/n.rs"]);
            land(&fixture, &repo, &no_diff, &["src/n.rs"], now);
            let bulk: Vec<String> = (0..13).map(|n| format!("bulk/{n}.rs")).collect();
            piloted(
                "bulk",
                &bulk.iter().map(String::as_str).collect::<Vec<_>>(),
                luna,
                &[],
                &["dir:bulk"],
                at,
                now,
            );
            let unassessed = add_with_status(&fixture, "unassessed", &[], vec![], TaskStatus::Done);
            land(&fixture, &repo, &unassessed, &["src/u.rs"], now);
            piloted(
                "old",
                &["src/old.rs"],
                luna,
                &[],
                &["file:src/other.rs"],
                now - Duration::days(40),
                now - Duration::days(40),
            );

            let card = scorecard(&fixture, "30d");
            assert_eq!(card["status"], "ok", "{card}");
            let units: Vec<(&str, &str, u64)> = card["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    (
                        row["crew"].as_str().unwrap(),
                        row["model"].as_str().unwrap(),
                        row["units"].as_u64().unwrap(),
                    )
                })
                .collect();
            assert_eq!(units.len(), 3, "{units:?}");
            for expected in [
                ("luna", "luna-model", 2),
                ("haiku", "claude-haiku-5-5", 2),
                ("crew unknown", "model unknown", 1),
            ] {
                assert!(units.contains(&expected), "{expected:?} in {units:?}");
            }

            let luna_row = row(&card, "luna", "luna-model");
            assert_eq!(
                stat(luna_row, "recall_percent"),
                100,
                "dir, wildcard, mirrored and golden files are covered: {luna_row}"
            );
            assert_eq!(stat(luna_row, "precision_percent"), 80, "4 of 5 selectors");
            assert_eq!(stat(luna_row, "f1_percent"), 88.89);
            assert_eq!(stat(luna_row, "filer_f1_percent"), 22.22);
            assert_eq!(stat(luna_row, "missed_files"), 0);
            assert_eq!(stat(luna_row, "unused_selectors"), 1);
            assert_eq!(
                luna_row["counts"]["f1_lift"], 1.4,
                "per-task lifts of 0.4 and 1.0"
            );

            let haiku_row = row(&card, "haiku", "claude-haiku-5-5");
            assert_eq!(stat(haiku_row, "recall_percent"), 66.67);
            assert_eq!(stat(haiku_row, "precision_percent"), 66.67);
            assert_eq!(stat(haiku_row, "filer_f1_percent"), 66.67);
            assert_eq!(
                haiku_row["counts"]["f1_lift"], 0.5,
                "a 0.5 loss on one task and a full gain on the legacy receipt"
            );
            assert!(
                as_f64(&luna_row["score"]) > as_f64(&haiku_row["score"]),
                "the larger lift ranks higher"
            );

            let facts = fixture.runtime.agent_scan().unwrap();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let detail = agent_detail(
                &facts,
                AgentRole::Pilots,
                &query,
                "haiku",
                Some("claude-haiku-5-5"),
                StdDuration::ZERO,
            )
            .unwrap();
            let lifted = &term(&detail, "f1_lift")["tasks"];
            let half_right = lifted
                .as_array()
                .unwrap()
                .iter()
                .find(|task| task["task_id"] == missed)
                .unwrap_or_else(|| panic!("the half-right task: {lifted}"));
            assert_eq!(half_right["count"], -0.5);
            let occurrence = half_right["occurrences"][0]["detail"].as_str().unwrap();
            assert!(
                occurrence.contains("src/y.rs") && occurrence.contains("src/z.rs"),
                "the detail names the missed file and the unused selector: {occurrence}"
            );

            let wide = scorecard(&fixture, "90d");
            assert_eq!(
                row(&wide, "luna", "luna-model")["units"],
                3,
                "the 40-day-old assessment counts only in a window that reaches it"
            );

            let server = fixture.server(false);
            let http = json_ok(server.get(&format!(
                "/api/agents/pilots/scorecard?{WS}&window=30d&min_support=1"
            )));
            assert_eq!(http["status"], "ok", "{http}");
            assert_eq!(http["rows"], card["rows"]);
        },
    );
}

/// A done, assessed task: `complexity`, a pilot receipt from `run` naming
/// `after`, an implementation run priced at `cost`, and `files` landed.
#[allow(clippy::too_many_arguments)]
fn calibrated(
    fixture: &Fixture,
    repo: &std::path::Path,
    title: &str,
    complexity: TaskComplexity,
    run: &str,
    files: &[&str],
    cost: f64,
    now: DateTime<Utc>,
) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "pilot scorecard fixture".into(),
            complexity,
            status: Some(TaskStatus::Done),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let selectors: Vec<String> = files.iter().map(|file| format!("file:{file}")).collect();
    let selectors: Vec<&str> = selectors.iter().map(String::as_str).collect();
    pilot_receipt(
        fixture,
        &id,
        now - Duration::hours(4),
        Some(run),
        &[],
        &selectors,
    );
    let implementation = format!("jrun-implement-{title}");
    let start = now - Duration::hours(3);
    implementation_run(
        fixture,
        &implementation,
        &id,
        "crew",
        Some("implement-model"),
        start,
        now - Duration::hours(2),
    );
    append_event(
        fixture,
        &id,
        start,
        "started",
        None,
        Some(TaskStatus::InProgress),
    );
    fixture
        .runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: implementation,
            account: None,
            activity_id: "implement_one".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("implement-model".into()),
            task_ids: vec![id.clone()],
            trace: InvocationTrace {
                provider_cost_usd: Some(cost),
                ..Default::default()
            },
        })
        .unwrap();
    land(fixture, repo, &id, files, now - Duration::hours(1));
    id
}

/// A lock wait on a path the holder reserved only through a pilot selector
/// that covered none of its landed files is charged to the holder's pilot;
/// a wait on a path a useful selector reserved is not. An uncovered landed
/// file the task's own lock request was refused on is a costly miss, one
/// nothing contested is not. The observed tier cuts the cost ranking in the
/// assessed proportions, and calibration counts the matrix diagonal.
#[test]
fn pilots_charge_wasted_reservations_costly_misses_and_calibrate_tiers() {
    isolated(
        "agents_pilots::pilots_charge_wasted_reservations_costly_misses_and_calibrate_tiers",
        || {
            let fixture = Fixture::new();
            let repo = init_repo(&fixture);
            let now = Utc::now();
            for (run, crew) in [("jrun-pilot-a", "pilot-a"), ("jrun-pilot-b", "pilot-b")] {
                seed_pilot_run(
                    &fixture,
                    run,
                    crew,
                    &format!("model-{}", &crew[6..]),
                    now - Duration::hours(9),
                    now - Duration::hours(8),
                );
            }

            // The holder reserves both pilot selectors, then lands a diff
            // that only one of them covers, plus two files neither covers.
            let holder = add_with_status(&fixture, "holder", &[], vec![], TaskStatus::Done);
            pilot_receipt(
                &fixture,
                &holder,
                now - Duration::hours(7),
                Some("jrun-pilot-a"),
                &[],
                &["file:src/held.rs", "file:src/core.rs"],
            );
            reserve(
                &fixture,
                "res-holder",
                &holder,
                &["src/held.rs", "src/core.rs"],
                now - Duration::hours(6),
            );
            let wasted = add(&fixture, "waits on the unused selector", &[], vec![]);
            let useful = add(&fixture, "waits on the useful selector", &[], vec![]);
            denial(
                &fixture,
                "denial-wasted",
                &wasted,
                "src/held.rs",
                ("reservation", "res-holder"),
                now - Duration::hours(5),
            );
            reserve_request(
                &fixture,
                "res-wasted",
                &[&wasted],
                None,
                &["src/held.rs"],
                now - Duration::hours(3),
            );
            denial(
                &fixture,
                "denial-useful",
                &useful,
                "src/core.rs",
                ("task", &holder),
                now - Duration::hours(5),
            );
            reserve(
                &fixture,
                "res-useful",
                &useful,
                &["src/core.rs"],
                now - Duration::hours(4),
            );
            denial(
                &fixture,
                "denial-holder",
                &holder,
                "src/missed.rs",
                ("task", &useful),
                now - Duration::minutes(330),
            );
            land(
                &fixture,
                &repo,
                &holder,
                &["src/core.rs", "src/missed.rs", "src/quiet.rs"],
                now - Duration::hours(1),
            );

            // Two low and two hard tasks: pilot-a ordered its two right,
            // pilot-b swapped its pair.
            let low = TaskComplexity::Low;
            let hard = TaskComplexity::Hard;
            calibrated(
                &fixture,
                &repo,
                "t1",
                low,
                "jrun-pilot-a",
                &["src/t1.rs"],
                1.0,
                now,
            );
            calibrated(
                &fixture,
                &repo,
                "t2",
                low,
                "jrun-pilot-b",
                &["src/t2.rs"],
                5.0,
                now,
            );
            calibrated(
                &fixture,
                &repo,
                "t3",
                hard,
                "jrun-pilot-b",
                &["src/t3.rs"],
                3.0,
                now,
            );
            calibrated(
                &fixture,
                &repo,
                "t4",
                hard,
                "jrun-pilot-a",
                &["src/t4.rs"],
                10.0,
                now,
            );

            let card = scorecard(&fixture, "30d");
            assert_eq!(card["status"], "ok", "{card}");
            let a = row(&card, "pilot-a", "model-a");
            let b = row(&card, "pilot-b", "model-b");
            assert_eq!((&a["units"], &b["units"]), (&json!(3), &json!(2)));
            assert_eq!(
                a["counts"]["lock_wait_hours"], 2,
                "the two-hour wait on the unused selector, not the hour on the useful one"
            );
            assert_eq!(
                a["counts"]["costly_misses"], 1,
                "src/missed.rs was contested; src/quiet.rs was not"
            );
            assert_eq!(b["counts"]["lock_wait_hours"], 0);
            assert_eq!(b["counts"]["costly_misses"], 0);

            let facts = fixture.runtime.agent_scan().unwrap();
            let query = AgentQuery::parse(Some("30d"), Some(1)).unwrap();
            let detail = agent_detail(
                &facts,
                AgentRole::Pilots,
                &query,
                "pilot-a",
                Some("model-a"),
                StdDuration::ZERO,
            )
            .unwrap();
            let waits = &term(&detail, "lock_wait_hours")["tasks"];
            assert_eq!(waits.as_array().unwrap().len(), 1, "{waits}");
            assert_eq!(waits[0]["task_id"], holder, "charged to the holder");
            let costly = &term(&detail, "costly_misses")["tasks"];
            assert_eq!(costly[0]["task_id"], holder, "{costly}");

            assert_eq!(a["counts"]["tier_calibration"], 2);
            assert_eq!(b["counts"]["tier_calibration"], 0);
            let matrix_a = stat(a, "tier_matrix");
            let matrix_b = stat(b, "tier_matrix");
            assert_eq!(matrix_a["tiers"], json!(["low", "hard"]), "{matrix_a}");
            assert_eq!(
                matrix_a["rows"],
                json!([
                    {"assessed": "low", "observed": [1, 0]},
                    {"assessed": "hard", "observed": [0, 1]},
                ])
            );
            assert_eq!(
                (
                    &matrix_a["placed"],
                    &matrix_a["not_placed"],
                    &matrix_a["on_tier"]
                ),
                (&json!(2), &json!(1), &json!(2)),
                "the holder has no assessed tier: {matrix_a}"
            );
            assert_eq!(
                matrix_b["rows"],
                json!([
                    {"assessed": "low", "observed": [0, 1]},
                    {"assessed": "hard", "observed": [1, 0]},
                ])
            );
            assert_eq!(
                (&matrix_b["too_easy"], &matrix_b["too_hard"]),
                (&json!(1), &json!(1)),
                "{matrix_b}"
            );
            // The observed tiers keep the assessed proportions.
            let mut assessed = [0u64; 2];
            let mut observed = [0u64; 2];
            for matrix in [&matrix_a, &matrix_b] {
                for (tier, row) in matrix["rows"].as_array().unwrap().iter().enumerate() {
                    for (column, count) in row["observed"].as_array().unwrap().iter().enumerate() {
                        let count = count.as_u64().unwrap();
                        assessed[tier] += count;
                        observed[column] += count;
                    }
                }
            }
            assert_eq!(assessed, observed);

            let server = fixture.server(false);
            let strip = json_ok(server.get(&format!("/api/agents/attribution?{WS}&window=30d")));
            assert_eq!(strip["owners"]["pilots"], 2, "{strip}");
            assert_eq!(
                strip["charged"]["pilots"],
                json!([{"crew": "pilot-a", "model": "model-a", "problems": 2}]),
                "{strip}"
            );
        },
    );
}

fn as_f64(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("a number: {value}"))
}
