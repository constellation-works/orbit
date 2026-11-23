//! The tool list a resident cycle receives follows its live grant: a right
//! that reaches `orbit.workflow` or `orbit.task.recover` puts that tool in the
//! list (ORB-27590), and each mapped action still meets the cycle guard's
//! live-grant, workspace and recovery gates when it is called.

use orbit_engine::{RuntimeHost, load_activity_asset};

use super::*;

/// The shipped cycle activity's disallow list.
fn cycle_disallow_list() -> Vec<String> {
    let activity = load_activity_asset(include_str!(
        "../../../assets/activities/resident_cycle.yaml"
    ))
    .unwrap();
    match activity.spec.spec {
        orbit_types::workflow::ActivityV2Spec::AgentLoop(spec) => spec
            .tool_disallow_list
            .expect("the cycle runs in deny mode"),
        other => panic!("the cycle is an agent loop: {other:?}"),
    }
}

/// The tools the host stamps for the cycle of `mandate`: its granted names
/// (which the nested server advertises) and its callable set.
fn cycle_tools(fx: &Fixture, mandate: &str) -> (Vec<String>, Vec<String>) {
    let resolved = fx
        .runtime
        .resolve_activity_tool_denials(
            &[mandate.to_string()],
            "resident_cycle",
            &cycle_disallow_list(),
        )
        .unwrap();
    (resolved.granted_tools, resolved.effective_tools)
}

#[test]
fn the_cycle_tool_list_follows_the_rights_its_grant_holds() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::cycle_tools::the_cycle_tool_list_follows_the_rights_its_grant_holds",
    ) {
        return;
    }
    let fx = fixture();
    let task = fx.task("Watched task");
    let cases: [(&[&str], &[&str]); 7] = [
        (&["promote", "plugin_upgrade"], &[]),
        (&["dispatch"], &["orbit.task.recover", "orbit.workflow"]),
        (&["merge"], &["orbit.workflow"]),
        (&["refresh"], &["orbit.workflow"]),
        (&["recover"], &["orbit.task.recover"]),
        (
            &["dispatch", "merge", "refresh", "recover"],
            &["orbit.task.recover", "orbit.workflow"],
        ),
        (&["promote"], &[]),
    ];
    for (rights, expected) in cases {
        let mandate = fx.mandate(&task);
        fx.grant(&mandate, rights);
        let run = fx.cycle_run(&mandate, &format!("cycle-tools-{}", rights.join("-")));
        fx.begin_cycle(&run, Utc::now());
        let (granted, effective) = cycle_tools(&fx, &mandate);
        let resident_granted: Vec<&str> = granted
            .iter()
            .map(String::as_str)
            .filter(|tool| matches!(*tool, "orbit.workflow" | "orbit.task.recover"))
            .collect();
        assert_eq!(resident_granted, expected, "rights {rights:?}: {granted:?}");
        // The shipped disallow list still covers what no right reaches.
        assert!(
            !effective
                .iter()
                .any(|tool| tool == "orbit.task.recover:review_reset"),
            "review_reset stays denied for {rights:?}"
        );
        assert!(
            !effective
                .iter()
                .any(|tool| tool.starts_with("orbit.auto_task:add")),
            "auto-task writes stay denied for {rights:?}"
        );
        fx.runtime.finish_resident_cycle(&run).unwrap();
    }
}

#[test]
fn a_cycle_without_a_live_grant_receives_neither_guarded_tool() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::cycle_tools::a_cycle_without_a_live_grant_receives_neither_guarded_tool",
    ) {
        return;
    }
    let fx = fixture();
    let task = fx.task("Watched task");
    let mandate = fx.mandate(&task);
    fx.grant(&mandate, &["dispatch", "merge", "refresh", "recover"]);

    // No cycle has begun: the grant alone puts nothing in the list.
    let (granted, _) = cycle_tools(&fx, &mandate);
    assert!(
        granted
            .iter()
            .all(|tool| !tool.starts_with("orbit.workflow")
                && !tool.starts_with("orbit.task.recover")),
        "{granted:?}"
    );

    // A cycle that finished holds no live grant either.
    let run = fx.cycle_run(&mandate, "cycle-tools-finished");
    fx.begin_cycle(&run, Utc::now());
    fx.runtime.finish_resident_cycle(&run).unwrap();
    let (granted, _) = cycle_tools(&fx, &mandate);
    assert!(
        granted
            .iter()
            .all(|tool| !tool.starts_with("orbit.workflow")
                && !tool.starts_with("orbit.task.recover")),
        "{granted:?}"
    );
}

#[cfg(unix)]
#[test]
fn each_mapped_action_keeps_the_live_grant_workspace_and_recovery_gates() {
    if !super::super::dispatch_admission::isolated(
        "resident_cycle::cycle_tools::each_mapped_action_keeps_the_live_grant_workspace_and_recovery_gates",
    ) {
        return;
    }
    let fx = fixture();
    let workspace = fx.runtime.workspace_id().unwrap();
    let task = fx.task("Watched task");
    let mapped = |workspace: &str| -> Vec<(&'static str, Value)> {
        vec![
            (
                "orbit.workflow.ship",
                json!({"task_ids": [task], "workspace": workspace}),
            ),
            (
                "orbit.workflow.land",
                json!({"id": task, "workspace": workspace}),
            ),
            (
                "orbit.workflow.refresh",
                json!({"id": task, "workspace": workspace}),
            ),
            (
                "orbit.task.repilot",
                json!({"id": task, "workspace": workspace, "model": "codex"}),
            ),
            (
                "orbit.task.resume_delivery",
                json!({"id": task, "run_id": "jrun-unobserved",
                       "diagnosis": "Repair the preserved change", "workspace": workspace}),
            ),
        ]
    };

    // A live cycle holding no right a mapped action needs.
    let promote_only = fx.mandate(&task);
    fx.grant(&promote_only, &["promote"]);
    let run = fx.cycle_run(&promote_only, "mapped-no-right");
    fx.begin_cycle(&run, Utc::now());
    fx.as_brokered_cycle(&promote_only, &run, || {
        for (tool, input) in mapped(&workspace) {
            let error = fx.agent_call(tool, input).expect_err(tool);
            assert!(error.contains("resident_right_refused"), "{tool}: {error}");
        }
        // A workspace outside the grant is refused before the right is read.
        for (tool, input) in mapped("ws_not_granted") {
            let error = fx.agent_call(tool, input).expect_err(tool);
            assert!(error.contains("resident_scope_refused"), "{tool}: {error}");
        }
    });

    // A cycle holding every right still needs a live cycle and its workspace.
    let all = fx.mandate(&task);
    fx.grant(&all, &["dispatch", "merge", "refresh", "recover"]);
    let run = fx.cycle_run(&all, "mapped-all-rights");
    fx.begin_cycle(&run, Utc::now());
    fx.as_brokered_cycle(&all, &run, || {
        for (tool, input) in mapped("ws_not_granted") {
            let error = fx.agent_call(tool, input).expect_err(tool);
            assert!(error.contains("resident_scope_refused"), "{tool}: {error}");
        }
    });
    fx.runtime.finish_resident_cycle(&run).unwrap();
    fx.as_brokered_cycle(&all, &run, || {
        for (tool, input) in mapped(&workspace) {
            let error = fx.agent_call(tool, input).expect_err(tool);
            assert!(error.contains("resident_cycle_stale"), "{tool}: {error}");
        }
    });
}
