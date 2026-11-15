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
    assert_follower_claim_holds_admission_until_released(&[]);
}

#[test]
fn explicit_no_diff_task_waits_for_follower_claim_until_released() {
    if !isolated(
        "dispatch_admission::follower_claims::explicit_no_diff_task_waits_for_follower_claim_until_released",
    ) {
        return;
    }
    assert_follower_claim_holds_admission_until_released(&["no-diff-expected"]);
    assert_no_diff_follower_claim_allows_admission(&["no-diff-expected"]);
}

fn assert_follower_claim_holds_admission_until_released(queued_tags: &[&str]) {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["shared.rs", "other.rs"]);
    // The frozen claim is authoritative even when its owner's task envelope
    // is backlog and declares a different surface. No local wrapper exists.
    let holder = seed(
        &runtime,
        Seed::named("follower holder").files(&["other.rs"]),
    );
    let queued = seed(
        &runtime,
        Seed::named("queued")
            .tags(queued_tags)
            .files(&["shared.rs"]),
    );
    let mut claim = seed_follower_claim(&runtime, &holder.id);
    let workspace_id = runtime.workspace_id().unwrap();
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
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
    if queued_tags.contains(&"no-diff-expected") {
        let payload: String = connection
            .query_row(
                "SELECT payload_json FROM task_coordination_rows
                 WHERE workspace_id=?1 AND kind='owner-wave-admission-v1' AND row_id=?2",
                rusqlite::params![workspace_id, queued.id],
                |row| row.get(0),
            )
            .unwrap();
        let wave: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(
            wave["footprint"],
            json!([]),
            "admitting no-diff work must not reserve its declared surface"
        );
    }
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

/// [ORB-23894] The work-start guard compares a task's whole context with a
/// claim's frozen footprint, unlocked Markdown included. A drain that judged
/// only the lock surface admitted the task every pass and worktree setup
/// failed it every time; it must wait instead, naming the claim, and be
/// admitted once the claim settles.
#[test]
fn follower_claim_over_unlocked_context_defers_admission_until_settled() {
    if !isolated(
        "dispatch_admission::follower_claims::follower_claim_over_unlocked_context_defers_admission_until_settled",
    ) {
        return;
    }
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["queued.rs", "other.rs", "design.md"]);
    let holder = seed(
        &runtime,
        Seed::named("follower holder").files(&["other.rs", "design.md"]),
    );
    let queued = seed(
        &runtime,
        Seed::named("queued").files(&["queued.rs", "design.md"]),
    );
    let mut claim =
        seed_follower_claim_over(&runtime, &holder.id, &["file:other.rs", "file:design.md"]);
    let workspace_id = runtime.workspace_id().unwrap();
    let connection = rusqlite::Connection::open(runtime.global_root().join("orbit.db")).unwrap();
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));
    let runs_before = runtime.list_job_runs(Default::default()).unwrap();

    for _ in 0..2 {
        let discovery = list_backlog_tasks(&runtime, json!({}));
        assert!(!admitted(&discovery).contains(&queued.id), "{discovery:#}");
        let excluded = excluded_entry(&discovery, &queued.id);
        assert_eq!(excluded["reason"], "context_lock_conflict", "{excluded}");
        assert_eq!(excluded["conflicts"][0]["locking_task_id"], holder.id);
        let explicit = list_backlog_tasks(&runtime, json!({"task_ids": [queued.id]}));
        assert!(admitted(&explicit).is_empty(), "{explicit:#}");
        assert!(
            excluded_entry(&explicit, &queued.id)["detail"]
                .as_str()
                .unwrap()
                .contains(&claim.run_context.run_id),
            "{explicit:#}"
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
        "a task the work-start guard refuses must create no run"
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
    assert!(admitted(&list_backlog_tasks(&runtime, json!({}))).contains(&queued.id));
    let wave = classify(&runtime, &drain);
    assert!(
        wave["loose_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(queued.id)),
        "{wave:#}"
    );
    assert_eq!(
        readiness_task(&default_readiness(&runtime), &queued.id)["reason"],
        "ready"
    );
}

#[test]
fn no_diff_follower_claim_does_not_hold_admission_or_readiness() {
    if !isolated(
        "dispatch_admission::follower_claims::no_diff_follower_claim_does_not_hold_admission_or_readiness",
    ) {
        return;
    }
    assert_no_diff_follower_claim_allows_admission(&[]);
}

fn assert_no_diff_follower_claim_allows_admission(queued_tags: &[&str]) {
    let (_root, runtime, repo) = runtime();
    write_files(&repo, &["shared.rs", "other.rs"]);
    let holder = seed(
        &runtime,
        Seed::named("no-diff follower holder")
            .tags(&["no-diff-expected"])
            .files(&["other.rs"]),
    );
    let queued = seed(
        &runtime,
        Seed::named("queued")
            .tags(queued_tags)
            .files(&["shared.rs"]),
    );
    // The nonempty frozen footprint overlaps the queued task even though the
    // holder's current declaration does not. The tag still exempts the holder.
    seed_follower_claim(&runtime, &holder.id);
    let drain = running_run(&runtime, "workspace_auto_pipeline", json!({}));

    let explicit = list_backlog_tasks(&runtime, json!({"task_ids": [queued.id]}));
    assert_eq!(admitted(&explicit), vec![queued.id.clone()], "{explicit:#}");
    let discovery = list_backlog_tasks(&runtime, json!({}));
    assert!(admitted(&discovery).contains(&queued.id), "{discovery:#}");
    assert!(
        !admitted(&discovery).contains(&holder.id),
        "the no-diff holder must not be dispatched twice: {discovery:#}"
    );
    let holder_explicit = list_backlog_tasks(&runtime, json!({"task_ids": [holder.id]}));
    assert!(admitted(&holder_explicit).is_empty(), "{holder_explicit:#}");
    let wave = classify(&runtime, &drain);
    assert!(
        wave["loose_task_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(queued.id)),
        "{wave:#}"
    );
    let readiness = default_readiness(&runtime);
    let entry = readiness_task(&readiness, &queued.id);
    assert_eq!(entry["eligible"], true, "{entry}");
    assert_eq!(entry["reason"], "ready", "{entry}");
    assert_eq!(
        readiness_task(&readiness, &holder.id)["eligible"],
        false,
        "a no-diff claim still holds its own task: {readiness:#}"
    );
}

/// A running follower claim with an expired reservation and a frozen surface
/// independent of its task's current declaration.
fn seed_follower_claim(runtime: &OrbitRuntime, task_id: &str) -> ExecutionClaim {
    seed_follower_claim_over(runtime, task_id, &["file:shared.rs"])
}

fn seed_follower_claim_over(
    runtime: &OrbitRuntime,
    task_id: &str,
    footprint: &[&str],
) -> ExecutionClaim {
    let claim = ExecutionClaim {
        claim_id: "fixture-follower-claim".into(),
        task_id: task_id.to_string(),
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
        footprint: footprint
            .iter()
            .map(|selector| selector.to_string())
            .collect(),
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
    claim
}
