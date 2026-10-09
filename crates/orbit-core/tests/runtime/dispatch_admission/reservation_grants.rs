//! A no-diff grant checks persistent holders while retaining no files.

use super::*;
use orbit_store::contracts::{
    AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
};

#[test]
fn no_diff_grants_check_frozen_claims_after_reservation_expiry() {
    if !isolated(
        "dispatch_admission::reservation_grants::no_diff_grants_check_frozen_claims_after_reservation_expiry",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    std::fs::write(repo.join("shared.txt"), "fixture\n").unwrap();
    let holder = seed(&runtime, Seed::default());
    let review = seed(
        &runtime,
        Seed {
            tags: &["no-diff-expected"],
            context_files: Some(&["file:shared.txt"]),
            ..Seed::default()
        },
    );
    // A durable claim snapshot whose original reservation has expired. Its
    // task is backlog on a different context, isolating the frozen footprint
    // from both active task envelopes and persistent file reservation rows.
    let mut claim = ExecutionClaim {
        claim_id: "fixture-claim".into(),
        task_id: holder.id.clone(),
        request_id: "fixture-request".into(),
        executed_on: ExecutionLocation {
            machine_id: "fixture-machine".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: "fixture-run".into(),
            job_name: "fixture".into(),
            machine_name: None,
        },
        footprint: vec!["file:shared.txt".into()],
        reservation_id: "reservation-expired-fixture".into(),
        reservation_expires_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
        phase: ExecutionClaimPhase::Running,
        repair: None,
    };
    let workspace_id = runtime.workspace_id().unwrap();
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection.execute(
        "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
         VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
        rusqlite::params![workspace_id, claim.claim_id, serde_json::to_string(&claim).unwrap(), Utc::now().to_rfc3339()],
    ).unwrap();
    let grant = |pipeline| {
        if pipeline {
            reserve_locks(&runtime, &review.id)
        } else {
            as_operator(
                &runtime,
                "orbit.task.locks.reserve",
                json!({"task_ids": [review.id]}),
            )
        }
    };
    for pipeline in [false, true] {
        let refused = grant(pipeline);
        assert_eq!(refused["reserved"], false, "{refused}");
        assert!(refused["reservation_id"].is_null());
        assert!(
            refused["conflicts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|conflict| {
                    conflict["file"] == "file:shared.txt" && conflict["held_by_id"] == holder.id
                }),
            "the expired reservation leaves its frozen footprint protected: {refused}"
        );
    }
    claim.phase = ExecutionClaimPhase::Revoked;
    connection
        .execute(
            "UPDATE task_coordination_rows SET payload_json=?1 WHERE workspace_id=?2 AND row_id=?3",
            rusqlite::params![
                serde_json::to_string(&claim).unwrap(),
                workspace_id,
                claim.claim_id
            ],
        )
        .unwrap();
    for pipeline in [false, true] {
        let granted = grant(pipeline);
        assert_eq!(granted["reserved"], true, "{granted}");
        assert_eq!(granted["reserved_files"], json!([]));
        assert!(granted["reservation_id"].is_string());
    }
}

#[test]
fn no_diff_grants_wait_for_persistent_holders_then_hold_no_files() {
    if !isolated(
        "dispatch_admission::reservation_grants::no_diff_grants_wait_for_persistent_holders_then_hold_no_files",
    ) {
        return;
    }
    for reservation_only_task in [false, true] {
        let (_root, runtime, repo) = runtime();
        std::fs::write(repo.join("shared.txt"), "fixture\n").unwrap();
        let review = seed(
            &runtime,
            Seed {
                tags: &["no-diff-expected"],
                context_files: Some(&["file:shared.txt"]),
                ..Seed::default()
            },
        );
        let ordinary = seed(
            &runtime,
            Seed {
                context_files: Some(&["file:shared.txt"]),
                ..Seed::default()
            },
        );
        let input = if reservation_only_task {
            // A backlog task has a reservation, but no active envelope lock.
            json!({"task_ids": [ordinary.id], "ttl_seconds": 120})
        } else {
            json!({"files": ["file:shared.txt"], "ttl_seconds": 120})
        };
        let holder = as_operator(&runtime, "orbit.task.locks.reserve", input);
        assert_eq!(holder["reserved"], true, "{holder}");
        assert_eq!(holder["reserved_files"], json!(["file:shared.txt"]));
        let holder_id = holder["reservation_id"].as_str().unwrap();
        let public_grant = || {
            as_operator(
                &runtime,
                "orbit.task.locks.reserve",
                json!({"task_ids": [review.id], "ttl_seconds": 120}),
            )
        };
        for refused in [public_grant(), reserve_locks(&runtime, &review.id)] {
            assert_eq!(refused["reserved"], false, "{refused}");
            assert!(refused["reservation_id"].is_null(), "{refused}");
            assert!(
                refused["conflicts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|conflict| {
                        conflict["held_by"] == "reservation" && conflict["held_by_id"] == holder_id
                    }),
                "the original context must see the persistent holder: {refused}"
            );
        }
        if reservation_only_task {
            // Move the fixture lease into the past without releasing it.
            // The grant must expire the row in its own transaction.
            let connection =
                rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
            let expires_at = Utc::now() - chrono::Duration::minutes(1);
            assert_eq!(connection.execute(
                "UPDATE task_reservations SET created_at=?1, expires_at=?2 WHERE reservation_id=?3",
                rusqlite::params![(expires_at - chrono::Duration::minutes(1)).to_rfc3339(), expires_at.to_rfc3339(), holder_id],
            ).unwrap(), 1);
        } else {
            let release = as_operator(
                &runtime,
                "orbit.task.locks.release",
                json!({"reservation_id": holder_id}),
            );
            assert_eq!(release["released"], true, "{release}");
        }
        for granted in [public_grant(), reserve_locks(&runtime, &review.id)] {
            assert_eq!(granted["reserved"], true, "{granted}");
            assert_eq!(granted["reserved_files"], json!([]), "{granted}");
            let id = granted["reservation_id"]
                .as_str()
                .expect("releasable empty grant");
            let listed = as_operator(&runtime, "orbit.task.locks", json!({}));
            let stored = listed["by_reservation"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["reservation_id"] == id)
                .unwrap();
            assert_eq!(stored["files"], json!([]), "{stored}");
        }
        let later = reserve_locks(&runtime, &ordinary.id);
        assert_eq!(
            later["reserved"], true,
            "empty grants do not block later work: {later}"
        );
        assert_eq!(later["reserved_files"], json!(["file:shared.txt"]));
        let explicit = as_operator(
            &runtime,
            "orbit.task.locks.reserve",
            json!({"files": ["file:shared.txt"]}),
        );
        assert_eq!(
            explicit["reserved"], false,
            "ordinary grants still exclude explicit files: {explicit}"
        );
    }
}

#[test]
fn closing_rescued_blocked_task_through_the_task_tool_succeeds_without_force_while_another_run_holds_overlapping_claim()
 {
    if !isolated(
        "dispatch_admission::reservation_grants::closing_rescued_blocked_task_through_the_task_tool_succeeds_without_force_while_another_run_holds_overlapping_claim",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    std::fs::write(repo.join("shared.txt"), "fixture\n").unwrap();

    let holder = seed(
        &runtime,
        Seed {
            title: "claim holder",
            status: TaskStatus::InProgress,
            context_files: Some(&["file:shared.txt"]),
            ..Seed::default()
        },
    );

    let claim = ExecutionClaim {
        claim_id: "claim-active".into(),
        task_id: holder.id.clone(),
        request_id: "req-active".into(),
        executed_on: ExecutionLocation {
            machine_id: "machine-1".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: "jrun-holder-42".into(),
            job_name: "test-job".into(),
            machine_name: None,
        },
        footprint: vec!["file:shared.txt".into()],
        reservation_id: "res-active".into(),
        reservation_expires_at: (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        phase: ExecutionClaimPhase::Running,
        repair: None,
    };
    let workspace_id = runtime.workspace_id().unwrap();
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    connection
        .execute(
            "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
            rusqlite::params![
                workspace_id,
                claim.claim_id,
                serde_json::to_string(&claim).unwrap(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
    let assert_names_claim = |refusal: &OrbitError, case: &str| {
        let message = refusal.to_string();
        assert!(
            message.contains("task footprint overlaps an execution claim")
                && message.contains(&holder.id)
                && message.contains("jrun-holder-42"),
            "{case}: the refusal must name the overlapping claim's task and run: {message}"
        );
    };

    let rescued = seed(
        &runtime,
        Seed {
            title: "rescued task",
            status: TaskStatus::Blocked,
            context_files: Some(&["file:shared.txt"]),
            ..Seed::default()
        },
    );
    let summary = "rescued work already landed by hand";
    let task_update = |capability, input: Value| {
        runtime.run_tool_with_context_and_role(
            "orbit.task.update",
            input,
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([capability]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
    };

    // Moving the blocked task into in-progress starts work on its files
    // unless an operator, naming no agent identity, closes it out with an
    // execution summary.
    for (case, capability, input) in [
        (
            "an agent session closing out",
            McpCapability::Agent,
            json!({"id": rescued.id, "status": "in-progress", "execution_summary": summary, "model": "claude"}),
        ),
        (
            "an operator session naming an agent model",
            McpCapability::Operator,
            json!({"id": rescued.id, "status": "in-progress", "execution_summary": summary, "model": "claude"}),
        ),
        (
            "an operator restart without an execution summary",
            McpCapability::Operator,
            json!({"id": rescued.id, "status": "in-progress"}),
        ),
    ] {
        let refusal = task_update(capability, input)
            .expect_err("a start of work on an overlapping claim's files is refused");
        assert_names_claim(&refusal, case);
        assert_eq!(
            runtime.get_task(&rescued.id).unwrap().status,
            TaskStatus::Blocked,
            "{case}: a refused start leaves the task blocked"
        );
    }

    // A drain-side start of other work on the same files is refused too.
    let candidate = seed(
        &runtime,
        Seed {
            title: "candidate to start",
            status: TaskStatus::Backlog,
            context_files: Some(&["file:shared.txt"]),
            ..Seed::default()
        },
    );
    let start_refusal = runtime
        .start_task(&candidate.id, None, None)
        .expect_err("start_task must be refused when footprint overlaps an active claim");
    assert_names_claim(&start_refusal, "a drain-side start");

    // The operator's close-out: blocked -> in-progress -> review -> done
    // through the registered tool, with no `force`.
    for (input, status) in [
        (
            json!({"id": rescued.id, "status": "in-progress", "execution_summary": summary}),
            "in-progress",
        ),
        (json!({"id": rescued.id, "status": "review"}), "review"),
        (json!({"id": rescued.id, "status": "done"}), "done"),
    ] {
        let written = as_operator(&runtime, "orbit.task.update", input);
        assert_eq!(written["status"], status, "{written}");
    }
    let closed = runtime.get_task(&rescued.id).unwrap();
    assert_eq!(closed.execution_summary, summary);
    assert!(
        !runtime
            .get_task_history(&rescued.id)
            .unwrap()
            .iter()
            .any(|entry| entry.event == "started" || entry.event == "forced"),
        "a rescue close neither starts work nor overrides the lifecycle"
    );
}
