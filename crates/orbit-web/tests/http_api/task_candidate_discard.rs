use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRunState, JobTargetType, TaskStatus};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, run_step, save_record_step};

fn task(fixture: &Fixture, status: TaskStatus, title: &str) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "candidate fixture".into(),
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn seed_candidate(fixture: &Fixture, id: &str, name: &str, state: JobRunState) {
    let mut run = fixture.seed_run(name, "task_pr_pipeline", state);
    run.input = Some(json!({"task_id": id, "task_ids": [id]}));
    fixture.save_run(&run);
    let mut pipeline = PipelineState::new(
        run.run_id.clone(),
        run.job_id.clone(),
        run.input.clone().unwrap(),
    );
    pipeline.pipeline["push"] = json!({"branch":"orbit/fixture", "head_sha":"abc"});
    fixture
        .runtime
        .write_run_state(&run.run_id, &pipeline)
        .unwrap();
}

#[test]
fn discard_candidate_from_open_states_and_refuse_live_run() {
    isolated(
        "task_candidate_discard::discard_candidate_from_open_states_and_refuse_live_run",
        || {
            let fixture = Fixture::new();
            let ids: Vec<_> = [
                TaskStatus::InProgress,
                TaskStatus::Review,
                TaskStatus::Backlog,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, status)| {
                let id = task(&fixture, status, &format!("candidate {i}"));
                seed_candidate(
                    &fixture,
                    &id,
                    &format!("jrun-published-{i}"),
                    JobRunState::Failed,
                );
                id
            })
            .collect();
            // This later delivery stopped at the candidate-resume gate. Held is a
            // terminal job state; it must not be treated as an executing worker.
            let mut held =
                fixture.seed_run("jrun-resume-held", "task_pr_pipeline", JobRunState::Held);
            held.input = Some(json!({"task_id": ids[0], "task_ids": [ids[0]]}));
            fixture.save_run(&held);
            fixture
                .runtime
                .apply_task_automation_update(
                    &ids[0],
                    TaskAutomationUpdate {
                        job_run_id: Some(held.run_id.clone()),
                        ..Default::default()
                    },
                )
                .unwrap();
            let mut step = run_step(
                JobTargetType::Activity,
                "resume_candidate",
                JobRunState::Failed,
            );
            step.error_code = Some("delivery_source_repair_decision".into());
            step.error_message = Some("published candidate lacks a unique requeue binding".into());
            save_record_step(&fixture, &held, &step);
            let empty = task(&fixture, TaskStatus::Backlog, "without candidate");
            let parent_running = task(&fixture, TaskStatus::InProgress, "live parent");
            seed_candidate(
                &fixture,
                &parent_running,
                "jrun-old-parent",
                JobRunState::Failed,
            );
            let mut parent = fixture.seed_run(
                "jrun-live-parent",
                "task_auto_pipeline",
                JobRunState::Running,
            );
            parent.input = Some(json!({"task_ids": [parent_running]}));
            fixture.save_run(&parent);
            let running = task(&fixture, TaskStatus::InProgress, "live candidate");
            seed_candidate(&fixture, &running, "jrun-live", JobRunState::Running);
            let ws = "?workspace=ws_http_fixture";
            let requeue = |id: &str| format!("/api/tasks/{id}/requeue{ws}");
            let detail = |id: &str| format!("/api/tasks/{id}{ws}&detail_candidate=true");
            let anon = fixture.server(false);
            error_code(
                anon.send(
                    "POST",
                    &requeue(&ids[0]),
                    json!({"reason":"fresh", "discard_candidate":true}),
                ),
                403,
                "authorization_denied",
            );
            let operator = fixture.server(true);
            let id = &ids[0];
            let plain: Value = operator
                .get(&format!("/api/tasks/{id}{ws}"))
                .json()
                .unwrap();
            assert!(plain.get("published_candidate").is_none());
            let page: Value = operator
                .get(&format!(
                    "/api/tasks{ws}&group=status&detail={id}&detail_candidate=true"
                ))
                .json()
                .unwrap();
            assert_eq!(page["detail"]["published_candidate"], true, "{page}");
            let no_candidate: Value = operator.get(&detail(&empty)).json().unwrap();
            assert_eq!(no_candidate["published_candidate"], false);
            for id in &ids {
                let before: Value = operator.get(&detail(id)).json().unwrap();
                assert_eq!(before["published_candidate"], true, "{before}");
                let response = operator.send(
                    "POST",
                    &requeue(id),
                    json!({"reason":"start over", "discard_candidate":true}),
                );
                assert_eq!(
                    response.status().as_u16(),
                    200,
                    "{}",
                    response.text().unwrap()
                );
                let task = fixture.runtime.get_task(id).unwrap();
                assert_eq!(task.status, TaskStatus::Backlog);
                let history = fixture.runtime.get_task_history(id).unwrap();
                assert!(
                    history
                        .iter()
                        .any(|e| e.by == "dashboard" && e.event == "candidate_discarded")
                );
                assert!(history.iter().any(|e| e.by == "dashboard"
                    && e.event == "operator_requeued"
                    && e.note.as_deref().unwrap_or_default().contains("start over")));
                assert!(
                    fixture
                        .runtime
                        .get_task_comments(id)
                        .unwrap()
                        .iter()
                        .any(|c| c.by == "dashboard" && c.message.contains("start over"))
                );
                let after: Value = operator.get(&detail(id)).json().unwrap();
                assert_eq!(after["published_candidate"], false, "{after}");
            }
            assert_eq!(
                fixture
                    .runtime
                    .show_job_run("jrun-resume-held")
                    .unwrap()
                    .state,
                JobRunState::Held
            );
            let parent_history = fixture.runtime.get_task_history(&parent_running).unwrap();
            error_code(
                operator.send(
                    "POST",
                    &requeue(&parent_running),
                    json!({"reason":"fresh", "discard_candidate":true}),
                ),
                409,
                "requeue_refused",
            );
            assert_eq!(
                fixture.runtime.get_task(&parent_running).unwrap().status,
                TaskStatus::InProgress
            );
            assert_eq!(
                fixture
                    .runtime
                    .get_task_history(&parent_running)
                    .unwrap()
                    .len(),
                parent_history.len()
            );
            let before = fixture.runtime.get_task(&running).unwrap();
            let history = fixture.runtime.get_task_history(&running).unwrap();
            error_code(
                operator.send(
                    "POST",
                    &requeue(&running),
                    json!({"reason":"fresh", "discard_candidate":true}),
                ),
                409,
                "requeue_refused",
            );
            let after = fixture.runtime.get_task(&running).unwrap();
            assert_eq!(after.status, before.status);
            assert_eq!(
                fixture.runtime.get_task_history(&running).unwrap().len(),
                history.len()
            );
            assert_eq!(
                fixture.runtime.show_job_run("jrun-live").unwrap().state,
                JobRunState::Running
            );
        },
    );
}

#[cfg(unix)]
#[test]
fn discard_closes_held_tasks_undelivered_pull_request() {
    use orbit_types::task::ExternalRef;
    use std::os::unix::fs::PermissionsExt;

    isolated(
        "task_candidate_discard::discard_closes_held_tasks_undelivered_pull_request",
        || {
            let fixture = Fixture::new();
            let id = task(&fixture, TaskStatus::InProgress, "held publication");
            seed_candidate(&fixture, &id, "jrun-source", JobRunState::Failed);
            let mut held = fixture.seed_run("jrun-held", "task_pr_pipeline", JobRunState::Held);
            held.input = Some(json!({"task_id": id, "task_ids": [id]}));
            fixture.save_run(&held);
            fixture
                .runtime
                .apply_task_automation_update(
                    &id,
                    TaskAutomationUpdate {
                        job_run_id: Some(held.run_id.clone()),
                        external_refs: vec![ExternalRef::github_pr("17".to_string()).unwrap()],
                        ..Default::default()
                    },
                )
                .unwrap();
            let bin = fixture.path("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let closed = fixture.path("closed-pr");
            let script = format!(
                r##"#!/bin/sh
case "$1 $2" in
 'pr list') printf '%s\n' '[{{"number":17,"headRefName":"orbit/{id}-aaaa1111","body":"Delivers {id}.","author":{{"login":"orbit-bot"}}}}]' ;;
 'api user') echo orbit-bot ;;
 'pr close') printf '%s\n' "$3" > '{closed}' ;;
 'pr view') echo '{{"number":17,"state":"OPEN"}}' ;;
 *) exit 1 ;;
esac
"##,
                closed = closed.display()
            );
            std::fs::write(bin.join("gh"), script).unwrap();
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let server = fixture.server_with_path(true, &bin);
            let response = server.send(
                "POST",
                &format!("/api/tasks/{id}/requeue?workspace=ws_http_fixture"),
                json!({"reason":"discard stale work", "discard_candidate":true}),
            );
            assert_eq!(response.status().as_u16(), 200);
            let body: Value = response.json().unwrap();
            assert_eq!(body["discard_closure"], "settled");
            assert_eq!(std::fs::read_to_string(closed).unwrap().trim(), "17");
            assert_eq!(
                fixture.runtime.get_task(&id).unwrap().status,
                TaskStatus::Backlog
            );
            let history = fixture.runtime.get_task_history(&id).unwrap();
            assert!(
                history
                    .iter()
                    .any(|event| event.event == "pull_request_closed")
            );
            assert!(history.iter().any(|event| {
                event.by == "dashboard"
                    && event
                        .note
                        .as_deref()
                        .unwrap_or_default()
                        .contains("discard stale work")
            }));
        },
    );
}
