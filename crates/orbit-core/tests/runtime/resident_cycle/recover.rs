//! A resident's `dispatch` right re-pilots and requeues through
//! `orbit.task.recover` (ORB-25959). Both were refused `capability_denied`
//! whatever the grant, because the operator row ran before the cycle guard, so
//! every re-pilot or requeue a resident needed became an escalation. The guard
//! that admits a cycle's ship now decides these two actions; every other
//! operator-only recover, run and pipeline tool stays refused.

use orbit_engine::{RuntimeHost, TaskAutomationUpdate};

use super::*;

/// `orbit.task.recover` actions other than `repilot` and `requeue`, and the
/// run and pipeline tools the `dispatch` row of the spec lists, with an input
/// each would otherwise accept.
fn operator_only_calls(task: &str, run: &str, workspace: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "orbit.task.recover_claim",
            json!({"id": task, "workspace": workspace}),
        ),
        (
            "orbit.task.retry_handoff",
            json!({"id": task, "workspace": workspace}),
        ),
        (
            "orbit.task.revoke_handoff",
            json!({"id": task, "workspace": workspace}),
        ),
        (
            "orbit.task.review_reset",
            json!({"id": task, "workspace": workspace}),
        ),
        (
            "orbit.task.reconcile_review",
            json!({"id": task, "workspace": workspace}),
        ),
        (
            "orbit.workflow.auto",
            json!({"mode": "status", "workspace": workspace}),
        ),
        (
            "orbit.workflow.run.resume",
            json!({"run_id": run, "workspace": workspace}),
        ),
        (
            "orbit.pipeline.invoke",
            json!({"job_id": "task_pilot_pipeline", "input": {"task_ids": [task]},
                   "workspace": workspace}),
        ),
    ]
}

fn blocked_task(fx: &Fixture, title: &str) -> String {
    let id = fx.task(title);
    fx.runtime
        .apply_task_automation_update(
            &id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                status_note: Some("escalated by its run".into()),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("block the task");
    id
}

fn repilot(fx: &Fixture, task: &str, workspace: &str) -> Result<Value, String> {
    fx.agent_call(
        "orbit.task.repilot",
        json!({"id": task, "workspace": workspace, "model": "codex"}),
    )
}

fn requeue(fx: &Fixture, task: &str, workspace: &str) -> Result<Value, String> {
    fx.agent_call(
        "orbit.task.requeue",
        json!({"id": task, "reason": "the outage that blocked it cleared",
               "workspace": workspace, "model": "codex"}),
    )
}

fn requeue_discarding_candidate(
    fx: &Fixture,
    task: &str,
    workspace: &str,
) -> Result<Value, String> {
    fx.agent_call(
        "orbit.task.requeue",
        json!({"id": task, "reason": "the outage that blocked it cleared",
               "workspace": workspace, "discard_candidate": true, "model": "codex"}),
    )
}

fn pilot_runs(fx: &Fixture) -> usize {
    fx.runtime
        .list_job_runs(orbit_core::application::job::JobRunListParams::default())
        .unwrap()
        .into_iter()
        .filter(|run| run.job_id == "task_pilot_pipeline")
        .count()
}

fn assert_ledgered(reply: &Value, mandate: &str) {
    assert!(
        reply["resident_action_key"]
            .as_str()
            .is_some_and(|key| key.starts_with(&format!("resident:{mandate}:"))),
        "the write is on the cycle's ledger: {reply}"
    );
}

fn assert_operator_refusal(error: &str, tool: &str) {
    assert!(
        error.starts_with("capability denied:"),
        "{tool} stays operator-only (capability_denied): {error}"
    );
}

#[cfg(unix)]
#[test]
fn a_cycle_with_dispatch_repilots_and_requeues_and_nothing_else_is_widened() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::recover::a_cycle_with_dispatch_repilots_and_requeues_and_nothing_else_is_widened",
    ) {
        return;
    }
    let fx = fixture();
    orbit_core::bootstrap::init::init_workspace_at_root(
        &fx.global,
        orbit_core::bootstrap::init::InitOptions {
            global_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker(["sh", "-c", "exit 0"]);
    let workspace = fx.runtime.workspace_id().unwrap();
    let proposed = fx.task("Re-pilot me");
    let held = blocked_task(&fx, "Requeue me");
    let mandate = fx.mandate(&proposed);
    fx.grant(&mandate, &["dispatch"]);
    let run = fx.cycle_run(&mandate, "resident-cycle-recover");
    fx.begin_cycle(&run, Utc::now());

    fx.as_brokered_cycle(&mandate, &run, || {
        // A workspace outside the grant is refused before anything runs.
        let error = repilot(&fx, &proposed, "ws_not_granted").expect_err("foreign workspace");
        assert!(error.contains("resident_scope_refused"), "{error}");
        let error = requeue(&fx, &held, "ws_not_granted").expect_err("foreign workspace");
        assert!(error.contains("resident_scope_refused"), "{error}");
        assert_eq!(pilot_runs(&fx), 0, "a refused re-pilot submits nothing");
        assert_eq!(
            fx.runtime.get_task(&held).unwrap().status,
            TaskStatus::Blocked
        );

        let task_before = fx.runtime.get_task(&held).unwrap();
        let history_before = fx.runtime.get_task_history(&held).unwrap();
        let error = requeue_discarding_candidate(&fx, &held, &workspace)
            .expect_err("resident requeues cannot discard a candidate");
        assert!(
            error.contains("capability denied:") && error.contains("discard_candidate"),
            "the typed refusal names the rejected parameter: {error}"
        );
        assert_eq!(
            fx.runtime.get_task(&held).unwrap(),
            task_before,
            "a refused discard does not change the task"
        );
        assert_eq!(
            fx.runtime.get_task_history(&held).unwrap(),
            history_before,
            "a refused discard leaves no requeue history"
        );

        let repiloted = repilot(&fx, &proposed, &workspace).expect("dispatch re-pilots");
        assert_ledgered(&repiloted, &mandate);
        assert_eq!(pilot_runs(&fx), 1, "{repiloted}");

        let requeued = requeue(&fx, &held, &workspace).expect("dispatch requeues");
        assert_ledgered(&requeued, &mandate);
        assert_eq!(
            fx.runtime.get_task(&held).unwrap().status,
            TaskStatus::Backlog,
            "{requeued}"
        );

        // The target task credits the run's agent identity, not the host's
        // process actor, and reads as a resident requeue, not a person's.
        let process_actor = fx.runtime.actor().resolve_write_label(None, None).unwrap();
        assert_ne!(
            process_actor, "codex",
            "the host's actor is a separate identity"
        );
        let history = fx.runtime.get_task_history(&held).unwrap();
        let entry = history
            .iter()
            .find(|entry| entry.event == "operator_requeued")
            .expect("requeue history entry");
        assert_eq!(entry.by, "codex", "{history:?}");
        let comments = fx.runtime.get_task_comments(&held).unwrap();
        let comment = comments
            .iter()
            .find(|comment| comment.message.starts_with("resident requeue"))
            .expect("resident requeue comment");
        assert_eq!(comment.by, "codex", "{comments:?}");
        assert!(
            comments.iter().all(|comment| comment.by != process_actor),
            "no comment credits the host actor: {comments:?}"
        );

        // Exactly these two actions are exempt: the right reaches no other
        // operator-only recover, run or pipeline tool.
        for (tool, input) in operator_only_calls(&held, &run, &workspace) {
            let error = fx.agent_call(tool, input).expect_err(tool);
            assert_operator_refusal(&error, tool);
        }
    });
    assert_eq!(
        pilot_runs(&fx),
        1,
        "only the granted re-pilot submitted a pilot"
    );
}

#[cfg(unix)]
#[test]
fn repilot_and_requeue_need_a_live_cycle_holding_dispatch() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::recover::repilot_and_requeue_need_a_live_cycle_holding_dispatch",
    ) {
        return;
    }
    let fx = fixture();
    let workspace = fx.runtime.workspace_id().unwrap();
    let proposed = fx.task("Re-pilot me");
    let held = blocked_task(&fx, "Requeue me");
    let both = |expected: &str| {
        let error = repilot(&fx, &proposed, &workspace).expect_err("re-pilot is refused");
        assert!(error.contains(expected), "repilot: {error}");
        let error = requeue(&fx, &held, &workspace).expect_err("requeue is refused");
        assert!(error.contains(expected), "requeue: {error}");
    };

    // A live cycle without the right.
    let promote_only = fx.mandate(&proposed);
    fx.grant(&promote_only, &["promote"]);
    let run = fx.cycle_run(&promote_only, "resident-cycle-no-dispatch");
    fx.begin_cycle(&run, Utc::now());
    fx.as_brokered_cycle(&promote_only, &run, || both("resident_right_refused"));

    // A cycle that holds the right but has finished.
    let dispatcher = fx.mandate(&held);
    fx.grant(&dispatcher, &["dispatch"]);
    let run = fx.cycle_run(&dispatcher, "resident-cycle-stale");
    fx.begin_cycle(&run, Utc::now());
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_brokered_cycle(&dispatcher, &run, || both("resident_cycle_stale"));

    // A managed run that is no cycle, and a session with no run binding, stay
    // held to the operator row.
    let KeyedJobRunAdmission::Admitted(other) = fx
        .jobs
        .insert_automation_job_run("agent_explore_pipeline", json!({}), "not-a-cycle")
        .unwrap()
    else {
        panic!("a fresh run key admits its run");
    };
    fx.as_cycle_agent(&other.run_id, || {
        let error = repilot(&fx, &proposed, &workspace).unwrap_err();
        assert_operator_refusal(&error, "repilot");
        let error = requeue(&fx, &held, &workspace).unwrap_err();
        assert!(
            error.contains("managed agents cannot requeue blocked tasks"),
            "{error}"
        );
    });
    both("requires the `operator` capability");

    assert_eq!(
        fx.runtime.get_task(&held).unwrap().status,
        TaskStatus::Blocked
    );
    assert_eq!(pilot_runs(&fx), 0, "no refused re-pilot submitted a pilot");
}

#[test]
fn environment_only_cycle_cannot_exercise_operator_rights() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::recover::environment_only_cycle_cannot_exercise_operator_rights",
    ) {
        return;
    }
    let fx = fixture();
    let task = fx.task("Watched task");
    let mandate = fx.mandate(&task);
    fx.grant(&mandate, &["dispatch", "merge"]);
    let run = fx.cycle_run(&mandate, "resident-scope-only");
    fx.begin_cycle(&run, Utc::now());
    let history = fx.runtime.get_task_history(&mandate).unwrap();
    fx.as_cycle_agent(&run, || {
        // No target is needed. Authorization must refuse before validating the
        // operation's input, submitting work, or opening a ledger entry.
        for tool in [
            "orbit.task.repilot",
            "orbit.task.requeue",
            "orbit.workflow.ship",
            "orbit.workflow.land",
        ] {
            let error = fx.agent_call(tool, json!({})).expect_err(tool);
            assert_operator_refusal(&error, tool);
        }
    });
    assert_eq!(fx.runtime.get_task_history(&mandate).unwrap(), history);
}
