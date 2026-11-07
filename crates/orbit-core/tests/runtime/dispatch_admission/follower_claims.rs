//! Follower claims hold their frozen surface before a local run is created.

use super::*;
use orbit_store::contracts::{
    AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase, ExecutionLocation,
};

#[test]
fn follower_claim_holds_admission_and_readiness_until_released() {
    if !isolated(
        "dispatch_admission::follower_claims::follower_claim_holds_admission_and_readiness_until_released",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["shared.rs", "other.rs"]);
    // The frozen claim is authoritative even when its owner's task envelope
    // is backlog and declares a different surface. No local wrapper exists.
    let holder = seed(
        &runtime,
        Seed::named("follower holder").files(&["other.rs"]),
    );
    let queued = seed(&runtime, Seed::named("queued").files(&["shared.rs"]));
    let mut claim = ExecutionClaim {
        claim_id: "fixture-follower-claim".into(),
        task_id: holder.id.clone(),
        request_id: "fixture-request".into(),
        executed_on: ExecutionLocation {
            machine_id: "fixture-follower".into(),
            machine_name: Some("follower-host".into()),
        },
        run_context: AdmissionRunContext {
            run_id: "fixture-follower-run".into(),
            job_name: "task_pipeline".into(),
            machine_name: Some("follower-host".into()),
        },
        footprint: vec!["file:shared.rs".into()],
        reservation_id: "expired-follower-reservation".into(),
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
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let runs_before = runtime.list_job_runs(Default::default()).unwrap();

    for _ in 0..2 {
        let explicit = list_backlog_tasks(&runtime, json!({"task_ids": [queued.id]}));
        assert!(admitted(&explicit).is_empty(), "{explicit:#}");
        let excluded = excluded_entry(&explicit, &queued.id);
        assert_eq!(excluded["reason"], "context_lock_conflict", "{excluded}");
        assert_eq!(excluded["conflicts"][0]["locking_task_id"], holder.id);
        assert!(
            excluded["detail"]
                .as_str()
                .unwrap()
                .contains(&claim.run_context.run_id),
            "{excluded}"
        );

        let discovery = list_backlog_tasks(&runtime, json!({}));
        assert!(!admitted(&discovery).contains(&queued.id), "{discovery:#}");
        assert_eq!(
            excluded_entry(&discovery, &queued.id)["reason"],
            "context_lock_conflict"
        );
        let wave = classify(&runtime, &drain);
        assert!(
            !wave["loose_task_ids"]
                .as_array()
                .unwrap()
                .contains(&json!(queued.id)),
            "{wave:#}"
        );
        let readiness = default_readiness(&runtime);
        let entry = readiness_task(&readiness, &queued.id);
        assert_eq!(entry["eligible"], false, "{entry}");
        assert_eq!(entry["reason"], "context_lock_conflict", "{entry}");
        assert_eq!(entry["blocking_claims"][0]["task_id"], holder.id, "{entry}");
        assert_eq!(
            entry["blocking_claims"][0]["run_id"], claim.run_context.run_id,
            "{entry}"
        );
    }
    assert_eq!(
        runtime.list_job_runs(Default::default()).unwrap(),
        runs_before,
        "held admission passes must create no run, including failed runs"
    );

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
    assert_eq!(
        admitted(&list_backlog_tasks(
            &runtime,
            json!({"task_ids": [queued.id]})
        )),
        vec![queued.id.clone()]
    );
    assert!(admitted(&list_backlog_tasks(&runtime, json!({}))).contains(&queued.id));
    let wave = classify(&runtime, &drain);
    assert!(
        wave["loose_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(queued.id)),
        "{wave:#}"
    );
    let readiness = default_readiness(&runtime);
    assert_eq!(readiness_task(&readiness, &queued.id)["reason"], "ready");
}
