//! Receipt-derived pilot identity through the public task-detail HTTP boundary.

use chrono::{Duration, SecondsFormat, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{InvocationInsertParams, TaskStatus};
use orbit_types::workflow::ActivityCrewDraw;
use serde_json::{Value, json};

use super::codebase::{pilot_receipt, pilot_run};
use super::support::{Fixture, isolated, json_ok};

#[test]
fn task_detail_attributes_the_producing_pilot_activity() {
    isolated(
        "task_hands::task_detail_attributes_the_producing_pilot_activity",
        || {
            let fixture = Fixture::new();
            let at = Utc::now() - Duration::minutes(5);
            let mut tasks = Vec::new();
            // Purpose beats legacy; missing evidence never borrows wrapper/task identity.
            for case in 0..9 {
                let task = fixture.runtime.add_task(TaskAddParams {
                title: format!("Pilot activity attribution {case}"),
                description: "A retained pilot receipt with a Terra wrapper, Haiku pilot and Sol implementer.".into(),
                status: Some(TaskStatus::Backlog),
                crew: Some("sol".into()),
                ..Default::default()
            }).unwrap();
                fixture
                    .runtime
                    .force_update_task_with_identity(
                        &task.id,
                        TaskUpdateParams {
                            implemented_by: Some(Some("sol".into())),
                            ..Default::default()
                        },
                        None,
                        None,
                    )
                    .unwrap();
                let run_id = format!("jrun-task-hands-{case}");
                pilot_run(
                    &fixture,
                    &run_id,
                    "terra",
                    "gpt-6.1-terra",
                    at - Duration::minutes(2),
                    at + Duration::minutes(1),
                );
                let mut state = fixture.runtime.read_run_state(&run_id).unwrap().unwrap();
                state.activity_crew_draws.clear();
                if case != 3 {
                    for (key, crew) in [
                        ("workflow.low_complexity_crews", "luna"),
                        ("workflow.purpose_crews.task_pilot", "haiku"),
                    ] {
                        if case == 1 && crew == "haiku" {
                            continue;
                        }
                        state.activity_crew_draws.insert(
                            key.into(),
                            ActivityCrewDraw {
                                crew: crew.into(),
                                source: key.into(),
                                eligible_pool: vec![],
                            },
                        );
                    }
                }
                pilot_receipt(
                    &fixture,
                    &task.id,
                    at,
                    Some(&run_id),
                    &[],
                    &["file:src/pilot.rs"],
                );
                let operation = fixture
                    .runtime
                    .get_task_comments(&task.id)
                    .unwrap()
                    .last()
                    .unwrap()
                    .receipt()
                    .unwrap()
                    .operation_id
                    .to_string();
                if case != 4 {
                    state.pipeline["apply"] =
                        json!({"tasks": [{"task_id": task.id, "operation_id": operation}]});
                }
                if case == 8 {
                    pilot_receipt(
                        &fixture,
                        &task.id,
                        at + Duration::seconds(1),
                        Some(&run_id),
                        &[],
                        &["file:src/pilot.rs"],
                    );
                    let comments = fixture.runtime.get_task_comments(&task.id).unwrap();
                    let repaired = comments.last().unwrap().receipt().unwrap().operation_id;
                    state.pipeline["apply_repairs"] =
                        json!({"tasks": [{"task_id": task.id, "operation_id": repaired}]});
                }
                fixture.runtime.write_run_state(&run_id, &state).unwrap();
                let invocations = match case {
                    2 => vec![("pilot", None, vec![task.id.to_string()])],
                    4 => vec![
                        ("pilot", Some("initial-model"), vec![task.id.to_string()]),
                        (
                            "repair_pilot",
                            Some("repair-model"),
                            vec![task.id.to_string()],
                        ),
                    ],
                    5 => vec![(
                        "repair_pilot",
                        Some("wrong-round-model"),
                        vec![task.id.to_string()],
                    )],
                    6 => vec![
                        ("pilot", Some("claude-haiku-5-5"), vec![task.id.to_string()]),
                        (
                            "pilot",
                            Some("unrelated-partition-model"),
                            vec!["HF-99999".into()],
                        ),
                    ],
                    7 => vec![
                        ("pilot", Some("ambiguous-a"), vec![]),
                        ("pilot", Some("ambiguous-b"), vec![]),
                    ],
                    8 => vec![
                        ("pilot", Some("claude-haiku-5-5"), vec![task.id.to_string()]),
                        (
                            "repair_pilot",
                            Some("claude-haiku-5-5-repair"),
                            vec![task.id.to_string()],
                        ),
                    ],
                    _ => vec![("pilot", Some("claude-haiku-5-5"), vec![task.id.to_string()])],
                };
                for (activity, model, task_ids) in invocations {
                    fixture
                        .runtime
                        .insert_invocation_trace_record(&InvocationInsertParams {
                            agent_execution_id: None,
                            job_run_id: run_id.clone(),
                            activity_id: activity.into(),
                            agent: "claude".into(),
                            provider: Some("claude".into()),
                            account: None,
                            model: model.map(str::to_string),
                            task_ids,
                            trace: Default::default(),
                        })
                        .unwrap();
                }
                tasks.push((task.id, run_id));
            }
            let server = fixture.server(false);
            let details: Vec<_> = tasks
                .iter()
                .map(|(id, _)| {
                    json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")))
                })
                .collect();
            // Optional evidence capture uses exactly the same isolated API data, before assertions.
            if let Some(dir) = std::env::var_os("ORBIT_TASK_HANDS_EVIDENCE_DIR") {
                let output = std::process::Command::new("node")
                    .arg(
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("tests/http_api/task_hands_browser.mjs"),
                    )
                    .arg(std::env::var_os("PLAYWRIGHT_MODULE").expect("browser kit module"))
                    .arg(dir)
                    .arg(&server.origin)
                    .arg(&tasks[0].0)
                    .output()
                    .expect("task hands browser fixture");
                assert!(
                    output.status.success(),
                    "browser fixture: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            for (case, detail) in details.iter().enumerate() {
                let hand = &detail["handled_by"][0];
                let model = match case {
                    2 | 4 | 5 | 7 => Value::Null,
                    _ => json!("claude-haiku-5-5"),
                };
                assert_eq!(
                    hand["crew"],
                    if case == 3 {
                        Value::Null
                    } else if case == 1 {
                        json!("luna")
                    } else {
                        json!("haiku")
                    },
                    "recorded pilot crew, case {case}: {detail}"
                );
                assert_eq!(hand["model"], model, "producing invocation, case {case}");
                assert_eq!(
                    hand["family"],
                    if model.is_null() {
                        Value::Null
                    } else {
                        json!("claude")
                    }
                );
                assert_eq!(hand["verb"], "piloted");
                assert_eq!(hand["run_id"], tasks[case].1);
                assert_eq!(hand["at"], at.to_rfc3339_opts(SecondsFormat::Secs, true));
                let count = if case == 8 { 2 } else { 1 };
                assert_eq!(detail["handled_by"].as_array().unwrap().len(), count);
                if case == 8 {
                    assert_eq!(
                        detail["handled_by"][1],
                        json!({
                            "verb": "piloted", "crew": "haiku", "model": "claude-haiku-5-5-repair",
                            "family": "claude", "run_id": tasks[case].1,
                            "at": (at + Duration::seconds(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                        })
                    );
                }
                assert_eq!(detail["implemented_by"], "sol");
                assert_eq!(
                    fixture
                        .runtime
                        .get_task_comments(&tasks[case].0)
                        .unwrap()
                        .len(),
                    count,
                    "reading historical evidence must not rewrite receipts"
                );
            }
        },
    );
}
