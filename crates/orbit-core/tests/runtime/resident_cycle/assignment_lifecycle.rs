//! Core owns the assignment's status across a mandate's life: `in-progress`
//! while active, `review` on a `handoff` checkpoint or a stop, whatever status
//! the assignment was left in.

use super::*;

fn status(fx: &Fixture, id: &str) -> TaskStatus {
    fx.runtime.get_task(id).unwrap().status
}

fn stop(fx: &Fixture, id: &str) -> Value {
    call(
        &fx.runtime,
        "orbit.resident.control",
        json!({"task_id": id, "action": "stop", "reason": "Enough for now"}),
        McpCapability::Operator,
    )
    .unwrap()
}

#[test]
fn attaching_a_valid_mandate_activates_a_proposed_assignment() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::assignment_lifecycle::attaching_a_valid_mandate_activates_a_proposed_assignment",
    ) {
        return;
    }
    let fx = fixture();
    let attach = |id: &str, body: &str| {
        let scratch = fx.repo.join(".orbit/tmp");
        std::fs::create_dir_all(&scratch).unwrap();
        let source = scratch.join("mandate.json");
        std::fs::write(&source, body).unwrap();
        call(
            &fx.runtime,
            "orbit.task.artifact.put",
            json!({"id": id, "path": "resident/mandate.json",
                   "source_path": source.display().to_string()}),
            McpCapability::Operator,
        )
        .unwrap()
    };
    let assignment = || {
        fx.runtime
            .add_task(TaskAddParams {
                title: "[resident]: scribe - sol".into(),
                tags: vec!["resident".into()],
                crew: Some("other".into()),
                ..Default::default()
            })
            .unwrap()
            .id
            .to_string()
    };

    let invalid = assignment();
    attach(&invalid, "{\"schema\": \"resident.mandate/v1\"}");
    assert_eq!(status(&fx, &invalid), TaskStatus::Proposed);

    let valid = assignment();
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Deliver scribe",
        "exit_criteria": ["Every scribe task is done"],
        "workspaces": [fx.runtime.workspace_id().unwrap()],
        "watch": {"tasks": [], "tags": ["scribe"]},
        "crew": "sol",
        "cadence": {"floor_minutes": 10, "heartbeat_minutes": 120}
    });
    let written = attach(&valid, &body.to_string());
    assert_eq!(status(&fx, &valid), TaskStatus::InProgress, "{written}");
    assert_eq!(
        fx.runtime.get_task(&valid).unwrap().crew.as_deref(),
        Some("sol")
    );
    let mut edited = body.clone();
    edited["crew"] = json!("other");
    attach(&valid, &edited.to_string());
    assert_eq!(
        fx.runtime.get_task(&valid).unwrap().crew.as_deref(),
        Some("other")
    );
}

#[test]
fn a_cycle_activates_an_assignment_attached_before_core_owned_its_status() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::assignment_lifecycle::a_cycle_activates_an_assignment_attached_before_core_owned_its_status",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched work");
    let mandate = fx.mandate(&watched);
    assert_eq!(status(&fx, &mandate), TaskStatus::Proposed);
    fx.complete_cycle(&mandate, "legacy-proposed");
    assert_eq!(status(&fx, &mandate), TaskStatus::InProgress);
}

#[test]
fn a_handoff_checkpoint_moves_the_assignment_to_review_and_ends_the_cycles() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::assignment_lifecycle::a_handoff_checkpoint_moves_the_assignment_to_review_and_ends_the_cycles",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched work");
    let mandate = fx.mandate(&watched);
    let run = fx.cycle_run(&mandate, "handoff-cycle");
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    assert_eq!(brief["cycle_count"], 1, "{brief}");

    // The agent cannot make the move itself, and the refusal names the route.
    let refused = fx
        .as_cycle_agent(&run, || {
            call(
                &fx.runtime,
                "orbit.task.update",
                json!({"id": mandate, "status": "review"}),
                McpCapability::Operator,
            )
        })
        .unwrap_err();
    assert!(
        refused.contains("resident_handoff_by_checkpoint"),
        "{refused}"
    );

    fx.checkpoint_with_outcome(&brief, &run, "handoff");
    let finished = fx.runtime.finish_resident_cycle(&run).unwrap();
    assert_eq!(finished["handoff"], "review", "{finished}");
    fx.stop_run(&run, JobRunState::Success);
    let task = fx.runtime.get_task(&mandate).unwrap();
    assert_eq!(task.status, TaskStatus::Review);
    assert!(
        task.execution_summary.contains("Nothing to do yet."),
        "the summary comes from the checkpoint: {}",
        task.execution_summary
    );
    assert_eq!(
        fx.runtime.show_resident_mandate(&mandate).unwrap()["state"],
        "handed_off"
    );

    // Nothing is due and no further cycle starts, even past its heartbeat.
    let later = Utc::now() + Duration::hours(3);
    assert!(fx.due_mandates(later).is_empty());
    let next = fx.cycle_run(&mandate, "after-handoff");
    let skipped = fx.runtime.begin_resident_cycle(&next, later).unwrap();
    assert_eq!(skipped["cycle_count"], 0, "{skipped}");
    assert_eq!(skipped["skip_reason"], "mandate_not_active", "{skipped}");
    fx.stop_run(&next, JobRunState::Success);

    // Reopening resumes the mandate instead of handing it off again.
    fx.runtime
        .update_task_as_human(
            &mandate,
            TaskUpdateParams {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    let reopened = fx.cycle_run(&mandate, "after-reopen");
    let resumed = fx.runtime.begin_resident_cycle(&reopened, later).unwrap();
    assert_eq!(resumed["cycle_count"], 1, "{resumed}");
}

#[test]
fn a_handoff_whose_finalize_never_ran_settles_instead_of_starting_a_cycle() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::assignment_lifecycle::a_handoff_whose_finalize_never_ran_settles_instead_of_starting_a_cycle",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched work");
    let mandate = fx.mandate(&watched);
    let run = fx.cycle_run(&mandate, "handoff-then-killed");
    let brief = fx.runtime.begin_resident_cycle(&run, Utc::now()).unwrap();
    fx.checkpoint_with_outcome(&brief, &run, "handoff");
    fx.stop_run(&run, JobRunState::Failed);

    let recovery = fx.cycle_run(&mandate, "handoff-recovery");
    let settled = fx
        .runtime
        .begin_resident_cycle(&recovery, Utc::now() + Duration::minutes(11))
        .unwrap();
    assert_eq!(settled["cycle_count"], 0, "{settled}");
    assert_eq!(settled["handoff"], "review", "{settled}");
    assert_eq!(status(&fx, &mandate), TaskStatus::Review);
    let shown = fx.runtime.show_resident_mandate(&mandate).unwrap();
    assert!(shown["live_cycle"].is_null(), "{shown}");
}

#[test]
fn a_stop_settles_an_assignment_from_any_open_status() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::assignment_lifecycle::a_stop_settles_an_assignment_from_any_open_status",
    ) {
        return;
    }
    let fx = fixture();
    let watched = fx.task("Watched work");
    let mandate = fx.mandate(&watched);
    assert_eq!(status(&fx, &mandate), TaskStatus::Proposed);
    let stopped = stop(&fx, &mandate);
    assert_eq!(stopped["handoff"], "review", "{stopped}");
    assert_eq!(status(&fx, &mandate), TaskStatus::Review);

    let blocked = fx.mandate(&watched);
    fx.runtime
        .update_task_as_human(
            &blocked,
            TaskUpdateParams {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
            "human:daniel".into(),
        )
        .unwrap();
    assert_eq!(stop(&fx, &blocked)["handoff"], "review");
    assert_eq!(status(&fx, &blocked), TaskStatus::Review);
}
