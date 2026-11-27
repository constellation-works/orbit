//! ORB-25105: a mandate with an empty watch list cycled at its floor on a
//! busy workspace, because any change to a task it had acted on (or any
//! notice) moved its wake digest. It now wakes only when an item it acts on
//! changes state, honours its checkpoint's planned next wake between the
//! floor and the heartbeat, and a mandate without a floor gets its heartbeat.

use super::*;
use orbit_core::app::resident;

/// A mandate with an empty watch list and the given cadence document.
fn unwatched_mandate(fx: &Fixture, cadence: Value) -> String {
    let id = fx.add_task(TaskAddParams {
        title: "[orchestration]: keep the workspace healthy - sol".into(),
        tags: vec!["resident".into()],
        ..Default::default()
    });
    let body = json!({
        "schema": "resident.mandate/v1",
        "goal": "Keep the workspace's delivery moving",
        "exit_criteria": ["The operator ends the mandate"],
        "workspaces": [fx.runtime.workspace_id().unwrap()],
        "crew": "sol",
        "cadence": cadence
    });
    fx.update_task(
        &id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "resident/mandate.json",
                body.to_string(),
            )],
            ..Default::default()
        },
    );
    id
}

/// One cycle that comments on `subject` (so the task becomes one it acted
/// on) and plans its next wake at `not_before`.
fn acting_cycle(fx: &Fixture, mandate: &str, key: &str, subject: &str, not_before: DateTime<Utc>) {
    let run = fx.cycle_run(mandate, key);
    let brief = fx.begin_cycle(&run, Utc::now());
    assert_eq!(brief["cycle_count"], 1, "{brief}");
    fx.as_cycle_agent(&run, || {
        fx.agent_call(
            "orbit.task.update",
            json!({"id": subject, "comment": "Checked its run.", "model": "codex"}),
        )
        .unwrap();
    });
    let row = fx.latest_action_row(mandate, brief["cycle"].as_str().unwrap());
    fx.write_checkpoint(
        &brief,
        &run,
        json!({
            "actions": [{"key": row.key, "kind": row.tool, "subject": subject,
                         "result": "ok", "ref": null}],
            "next_wake": {"reason": "planned", "not_before": not_before},
            "outcome": "progress"
        }),
    );
    fx.finish_cycle(&run);
}

/// Drain churn on `task`: edits and moves between active states.
fn churn(fx: &Fixture, task: &str, round: usize) {
    fx.update_task(
        task,
        TaskUpdateParams {
            description: Some(format!("Run attempt {round} retried.")),
            ..Default::default()
        },
    );
}

#[test]
fn an_unwatched_mandate_ignores_churn_and_cycles_at_its_heartbeat() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::wake_scope::an_unwatched_mandate_ignores_churn_and_cycles_at_its_heartbeat",
    ) {
        return;
    }
    let fx = fixture();
    let subject = fx.task("Repair the flaky export test");
    let mandate = unwatched_mandate(&fx, json!({"floor_minutes": 5, "heartbeat_minutes": 20}));
    let now = Utc::now();
    acting_cycle(
        &fx,
        &mandate,
        "resident-cycle-1",
        &subject,
        now + Duration::hours(2),
    );

    // A busy workspace: the task it acted on is edited and moved between
    // active states, and an info notice is posted. None of it is a state
    // the mandate acts on, so the 5-minute floor never becomes the interval.
    fx.update_task(
        &subject,
        TaskUpdateParams {
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        },
    );
    fx.operator_call(
        "orbit.notice.post",
        json!({"kind": "info", "title": "Drain running", "expires_in_hours": 1}),
    );
    for minute in 1..20 {
        churn(&fx, &subject, minute);
        assert!(
            !fx.due_mandates(now + Duration::minutes(minute as i64 + 1))
                .contains(&mandate),
            "ORB-25105: churn woke the mandate at +{}m",
            minute + 1
        );
    }
    // The planned wake two hours out is clamped to the 20-minute heartbeat:
    // at most three cycles an hour without a qualifying change.
    assert!(
        fx.due_mandates(now + Duration::minutes(21))
            .contains(&mandate),
        "the heartbeat makes it due"
    );

    // A qualifying change on the same task (it blocks) wakes it once the
    // floor allows, before the heartbeat.
    let blocked = unwatched_mandate(&fx, json!({"floor_minutes": 5, "heartbeat_minutes": 20}));
    let other = fx.task("Land the export PR");
    acting_cycle(
        &fx,
        &blocked,
        "resident-cycle-blocked",
        &other,
        now + Duration::hours(2),
    );
    assert!(
        !fx.due_mandates(now + Duration::minutes(6))
            .contains(&blocked)
    );
    fx.update_task(
        &other,
        TaskUpdateParams {
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        },
    );
    assert!(
        fx.due_mandates(now + Duration::minutes(6))
            .contains(&blocked),
        "a task it acted on blocking wakes it"
    );
}

#[test]
fn a_checkpoints_next_wake_is_honoured_between_floor_and_heartbeat() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::wake_scope::a_checkpoints_next_wake_is_honoured_between_floor_and_heartbeat",
    ) {
        return;
    }
    let fx = fixture();
    let subject = fx.task("Watch the nightly run");
    let now = Utc::now();
    let cadence = json!({"floor_minutes": 5, "heartbeat_minutes": 20});

    // Planned 12 minutes out: not due before, due after.
    let planned = unwatched_mandate(&fx, cadence.clone());
    acting_cycle(
        &fx,
        &planned,
        "resident-cycle-planned",
        &subject,
        now + Duration::minutes(12),
    );
    assert!(
        !fx.due_mandates(now + Duration::minutes(11))
            .contains(&planned)
    );
    assert!(
        fx.due_mandates(now + Duration::minutes(13))
            .contains(&planned),
        "the planned wake makes it due before the heartbeat"
    );

    // Planned sooner than the floor: the floor holds it.
    let eager = unwatched_mandate(&fx, cadence);
    acting_cycle(
        &fx,
        &eager,
        "resident-cycle-eager",
        &subject,
        now + Duration::minutes(1),
    );
    assert!(!fx.due_mandates(now + Duration::minutes(4)).contains(&eager));
    assert!(
        fx.due_mandates(now + Duration::minutes(6)).contains(&eager),
        "a wake planned before the floor fires at the floor"
    );
}

#[test]
fn a_mandate_without_a_floor_gets_its_heartbeat_as_floor() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::wake_scope::a_mandate_without_a_floor_gets_its_heartbeat_as_floor",
    ) {
        return;
    }
    let fx = fixture();
    let mandate = unwatched_mandate(&fx, json!({"heartbeat_minutes": 20}));
    let shown = resident::show_resident_mandate(&fx.runtime, &mandate).unwrap();
    assert_eq!(shown["state"], "active", "{shown}");
    assert_eq!(
        shown["mandate"]["cadence"],
        json!({"floor_minutes": 20, "heartbeat_minutes": 20}),
        "{shown}"
    );

    // A qualifying change soon after a cycle waits for the 20-minute floor.
    let subject = fx.task("Ship the release notes");
    let now = Utc::now();
    acting_cycle(
        &fx,
        &mandate,
        "resident-cycle-1",
        &subject,
        now + Duration::minutes(5),
    );
    fx.update_task(
        &subject,
        TaskUpdateParams {
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        },
    );
    assert!(
        !fx.would_fire(now + Duration::minutes(10)),
        "the floor equals the heartbeat"
    );
    assert!(fx.would_fire(now + Duration::minutes(21)));
}

#[test]
fn heartbeat_skips_unchanged_but_runs_when_watched_state_changes() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::wake_scope::heartbeat_skips_unchanged_but_runs_when_watched_state_changes",
    ) {
        return;
    }
    let fx = fixture();
    let (watched, mandate) = fx.watched_mandate("Heartbeat subject");
    let acted = fx.add_task(TaskAddParams {
        title: "Decide the next step".into(),
        description: "An acted-on subject".into(),
        acceptance_criteria: vec!["Decided".into()],
        crew: Some("dk".into()),
        ..Default::default()
    });
    let now = Utc::now();
    let first = fx.cycle_run(&mandate, "heartbeat-first");
    let brief = fx.begin_cycle(&first, now);
    fx.as_cycle_agent(&first, || {
        fx.agent_call(
            "orbit.task.update",
            json!({"id": acted, "comment": "Reviewed.", "model": "codex"}),
        )
        .unwrap();
    });
    let row = fx.latest_action_row(&mandate, brief["cycle"].as_str().unwrap());
    fx.write_checkpoint(&brief, &first, json!({
        "actions": [{"key": row.key, "kind": row.tool, "subject": acted, "result": "ok", "ref": null}],
        "conversation_ref": "conv-heartbeat", "notes": "Preserve this observation."
    }));
    fx.finish_cycle(&first);
    let skip = fx.cycle_run(&mandate, "heartbeat-skip");
    let result = fx.begin_cycle(&skip, now + Duration::hours(3));
    assert_eq!(result["cycle_count"], 0, "{result}");
    assert_eq!(result["skip_reason"], "unchanged", "{result}");
    let comments = fx.runtime.get_task_comments(&mandate).unwrap();
    let checkpoint = comments
        .iter()
        .filter_map(|comment| {
            match orbit_types::workflow::ResidentComment::parse(&comment.message).ok()? {
                orbit_types::workflow::ResidentComment::Checkpoint(checkpoint) => Some(checkpoint),
                _ => None,
            }
        })
        .next_back()
        .unwrap();
    assert_eq!(checkpoint.outcome, "unchanged");
    let shown = resident::show_resident_mandate(&fx.runtime, &mandate).unwrap();
    assert_eq!(shown["stalled"], false, "{shown}");
    fx.stop_run(&skip, JobRunState::Success);
    let request = fx
        .runtime
        .run_tool(
            "orbit.decision.raise",
            json!({
                "task_id": acted, "model": "codex", "question": "Which next step?",
                "options": [
                    {"label": "Proceed", "consequence": "Proceed now.", "recommended": true},
                    {"label": "Wait", "consequence": "Wait for later."}
                ]
            }),
        )
        .unwrap();
    fx.update_task(
        &watched,
        TaskUpdateParams {
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        },
    );
    let changed = fx.cycle_run(&mandate, "heartbeat-changed");
    let result = fx.begin_cycle(&changed, now + Duration::hours(6));
    assert_eq!(result["cycle_count"], 1, "{result}");
    assert_eq!(result["continuity"], "resumed", "{result}");
    assert_eq!(result["conversation_ref"], "conv-heartbeat");
    assert_eq!(result["checkpoints"][0]["cycle"], brief["cycle"]);
    assert_eq!(
        result["checkpoints"][0]["notes"],
        "Preserve this observation."
    );
    assert_eq!(result["checkpoints"][0]["actions"][0]["subject"], acted);
    assert!(
        result["open_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == request["id"]),
        "{result}"
    );
    assert!(
        result["wake"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "digest_changed")
    );

    let checkpoint = fx.write_checkpoint(
        &result,
        &changed,
        json!({
            "continuity": "resumed",
            "conversation_ref": "conv-heartbeat-next"
        }),
    );
    assert_eq!(
        checkpoint["checkpoint"]["continuity"], "resumed",
        "{checkpoint}"
    );
    let stored = fx
        .runtime
        .get_task_comments(&mandate)
        .unwrap()
        .iter()
        .filter_map(
            |comment| match ResidentComment::parse(&comment.message).ok()? {
                ResidentComment::Checkpoint(checkpoint)
                    if checkpoint.cycle == result["cycle"].as_str().unwrap() =>
                {
                    Some(checkpoint)
                }
                _ => None,
            },
        )
        .next_back()
        .unwrap();
    assert_eq!(stored.continuity, "resumed");
    assert_eq!(
        stored.conversation_ref.as_deref(),
        Some("conv-heartbeat-next")
    );
    fx.finish_cycle(&changed);
}
