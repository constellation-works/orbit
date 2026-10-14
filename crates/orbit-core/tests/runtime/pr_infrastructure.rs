//! Durable owner admission at the public host/store boundary.
use orbit_core::{OrbitRuntime, application::task::TaskAddParams};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::{
    PR_INFRASTRUCTURE_RESERVED_EVENT, PrInfrastructureAdmission, PrInfrastructureEvidence,
    pr_infrastructure_reservations,
};
use serde_json::json;

fn owner_reservation_survives_concurrent_sibling_and_reopened_store() {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let owner = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = owner
        .add_task(TaskAddParams {
            title: "Workflow prerequisite recovery".into(),
            ..Default::default()
        })
        .unwrap();
    let evidence: PrInfrastructureEvidence = serde_json::from_value(json!({
        "repository":"orbit/test","repository_id":500,"pr_number":"42","head_sha":"a".repeat(40),"head_branch":"candidate","base_branch":"agent-main","base_sha":"b".repeat(40),
        "required":[{"context":"protected","app_id":900}],"missing":{"context":"protected","app_id":900},"app_id":900,"workflow_id":600,"workflow_path":".github/workflows/ci.yml","workflow_source":"jobs: {}","workflow_blob":"c".repeat(40),"workflow_job":"protected","prerequisite":"state","prerequisite_job_id":1000,"run_id":700,"run_attempt":1,"event":"pull_request","policy":{"effective_rules":[]},"provider":{},"run":{"id":700},"jobs":{"total_count":1},"log":"gh: Service Unavailable (HTTP 503)"
    })).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let sibling = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let calls =
        [(owner.clone(), "source-run"), (sibling, "sibling-run")].map(|(host, execution)| {
            let barrier = barrier.clone();
            let evidence = evidence.clone();
            let id = task.id.to_string();
            std::thread::spawn(move || {
                barrier.wait();
                RuntimeHost::admit_pr_infrastructure(&host, execution, &[id], &evidence).unwrap()
            })
        });
    let outcomes = calls.map(|call| call.join().unwrap());
    assert_eq!(
        outcomes
            .iter()
            .filter(|answer| matches!(answer, PrInfrastructureAdmission::Reserved(_)))
            .count(),
        1,
        "the owner lock reserves before exactly one sibling can mutate"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|answer| matches!(answer, PrInfrastructureAdmission::Observe(_)))
            .count(),
        1
    );
    let reopened = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let ids = vec![task.id.to_string()];
    assert!(matches!(
        RuntimeHost::admit_pr_infrastructure(&reopened, "resumed-run", &ids, &evidence).unwrap(),
        PrInfrastructureAdmission::Observe(_)
    ));
    let history = reopened.get_task_history(&task.id).unwrap();
    let reservations = pr_infrastructure_reservations(&history).unwrap();
    assert_eq!(reservations.len(), 1);
    assert_eq!(
        reservations[0].evidence, evidence,
        "original policy/source/log evidence survives reopen"
    );
    let mut advanced = evidence.clone();
    advanced.run_attempt = 2;
    assert!(matches!(
        RuntimeHost::admit_pr_infrastructure(&reopened, "later-run", &ids, &advanced).unwrap(),
        PrInfrastructureAdmission::Refused(_)
    ));
    let mut next_head = evidence.clone();
    next_head.head_sha = "d".repeat(40);
    assert!(matches!(
        RuntimeHost::admit_pr_infrastructure(&reopened, "next-head", &ids, &next_head).unwrap(),
        PrInfrastructureAdmission::Refused(_)
    ));
    assert_eq!(
        reopened.get_task_history(&task.id).unwrap(),
        history,
        "observation/refusal never resets or consumes another attempt"
    );
    RuntimeHost::apply_task_automation_update(
        &owner,
        &task.id,
        TaskAutomationUpdate {
            append_history: vec![orbit_types::task::TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: "system".into(),
                event: PR_INFRASTRUCTURE_RESERVED_EVENT.into(),
                note: Some("broken".into()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        matches!(
            RuntimeHost::admit_pr_infrastructure(&reopened, "corrupt-ledger", &ids, &evidence)
                .unwrap(),
            PrInfrastructureAdmission::Refused(_)
        ),
        "a malformed ledger cannot grant a fresh budget"
    );
}

super::scenario_table!(
    durable_infrastructure_admission,
    owner_reservation_survives_concurrent_sibling_and_reopened_store
);
