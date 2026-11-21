//! A `ci_check_rerun_reserved` entry is read the same whether it carries the
//! plan as its typed decision (a one-line note beside it) or, as entries
//! written before [ORB-25127] do, as a serialized-JSON note.

use chrono::{DateTime, Utc};
use orbit_types::task::{TaskDecision, TaskHistoryEntry};
use orbit_types::workflow::{
    CI_RERUN_RESERVED_EVENT, CiInfrastructureRetryPolicy, CiRerunAdmission, CiRerunPlan,
    CiRerunRefusal, CiRerunRoot, decide_ci_rerun_with_policy,
};
use serde_json::json;

fn plan(classification: &str) -> CiRerunPlan {
    CiRerunPlan {
        schema_version: 1,
        repository: "owner/repo".into(),
        repository_id: 7,
        pr_number: "9384".into(),
        head_sha: "a".repeat(40),
        head_branch: "orbit/ORB-1".into(),
        base_branch: "agent-main".into(),
        accepted_base: "b".repeat(40),
        run_id: 43_585_519_019,
        run_attempt: 1,
        workflow_id: 11,
        roots: vec![CiRerunRoot {
            job_id: 100,
            check: "CI / Check / Clippy / Test".into(),
            classification: classification.into(),
            signature: "observed failure".into(),
        }],
        failed_jobs: vec![json!({"id": 100, "completed_at": "2026-11-14T11:50:00Z"})],
        authentication: json!({"run": {"jobs": vec!["x".repeat(64); 64]}, "logs": "y".repeat(4096)}),
    }
}

fn at(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap()
}

fn legacy_entry(plan: &CiRerunPlan, ms: i64) -> TaskHistoryEntry {
    TaskHistoryEntry::new(at(ms), "system", CI_RERUN_RESERVED_EVENT)
        .with_note(Some(serde_json::to_string(plan).unwrap()))
}

/// The entry as the store persists and reloads it.
fn reloaded(entry: &TaskHistoryEntry) -> TaskHistoryEntry {
    serde_json::from_str(&serde_json::to_string(entry).unwrap()).unwrap()
}

#[test]
fn a_reservation_note_is_one_line_and_the_decision_keeps_the_whole_plan() {
    let plan = plan("unattributed");
    let entry = reloaded(&plan.reservation_entry(at(1_000)));
    let note = entry.note.as_deref().unwrap();
    assert_eq!(
        note,
        "CI rerun reserved: 'CI / Check / Clippy / Test' on PR #9384 run 43585519019 attempt 1 (unattributed)"
    );
    assert_eq!(
        entry.decision,
        Some(TaskDecision::CiRerunReserved(Box::new(plan.clone())))
    );
    assert_eq!(
        CiRerunPlan::from_reservation_entry(&entry),
        Some(plan.clone())
    );

    let mut wide = plan;
    wide.roots = (0..40)
        .map(|job| CiRerunRoot {
            job_id: job,
            check: format!("CI / Test shard {job}\nwith a newline"),
            classification: "flaky".into(),
            signature: String::new(),
        })
        .collect();
    let note = wide.reservation_note();
    assert!(
        !note.contains('\n') && note.chars().count() <= 200,
        "{note}"
    );
}

#[test]
fn admission_decides_the_same_for_a_typed_and_a_legacy_reservation() {
    let policy = CiInfrastructureRetryPolicy::default();
    for classification in ["flaky", "transient", "unattributed"] {
        let reserved = plan(classification);
        let typed = vec![reloaded(&reserved.reservation_entry(at(1_000)))];
        let legacy = vec![reloaded(&legacy_entry(&reserved, 1_000))];
        assert_eq!(
            CiRerunPlan::from_reservation_entry(&typed[0]),
            CiRerunPlan::from_reservation_entry(&legacy[0])
        );
        let mut later = reserved.clone();
        later.run_attempt = 2;
        let mut other_head = reserved.clone();
        other_head.head_sha = "c".repeat(40);
        let mut conflicting = reserved.clone();
        conflicting.authentication = json!({"other": true});
        for candidate in [&reserved, &later, &other_head] {
            for now_ms in [1_000, 10_000_000] {
                let from_typed = decide_ci_rerun_with_policy(&typed, candidate, policy, now_ms);
                let from_legacy = decide_ci_rerun_with_policy(&legacy, candidate, policy, now_ms);
                assert_eq!(from_typed, from_legacy, "{classification} {now_ms}");
            }
        }
        assert_eq!(
            decide_ci_rerun_with_policy(&typed, &reserved, policy, 1_000),
            CiRerunAdmission::Observe(Box::new(reserved.clone()))
        );
        // A typed and a legacy reservation of the same attempt that disagree
        // conflict exactly as two legacy ones do.
        let mixed = vec![
            typed[0].clone(),
            reloaded(&legacy_entry(&conflicting, 2_000)),
        ];
        assert!(matches!(
            decide_ci_rerun_with_policy(&mixed, &reserved, policy, 3_000),
            CiRerunAdmission::Refused(CiRerunRefusal::ConflictingReservations)
        ));
    }
}

#[test]
fn a_foreign_decision_or_unreadable_note_is_no_reservation() {
    let foreign = TaskHistoryEntry::new(at(1_000), "system", CI_RERUN_RESERVED_EVENT)
        .with_note(Some(serde_json::to_string(&plan("flaky")).unwrap()))
        .with_decision(Some(TaskDecision::PilotUnresolved {
            pilot_run_id: "jrun-x".into(),
        }));
    assert_eq!(CiRerunPlan::from_reservation_entry(&foreign), None);
    let unreadable = TaskHistoryEntry::new(at(1_000), "system", CI_RERUN_RESERVED_EVENT)
        .with_note(Some("CI rerun reserved: one line".into()));
    assert_eq!(CiRerunPlan::from_reservation_entry(&unreadable), None);
    assert!(matches!(
        decide_ci_rerun_with_policy(
            &[unreadable],
            &plan("flaky"),
            CiInfrastructureRetryPolicy::default(),
            1_000
        ),
        CiRerunAdmission::Refused(CiRerunRefusal::UnreadableReservation)
    ));
}
