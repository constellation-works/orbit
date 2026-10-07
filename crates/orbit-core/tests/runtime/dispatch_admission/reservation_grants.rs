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
