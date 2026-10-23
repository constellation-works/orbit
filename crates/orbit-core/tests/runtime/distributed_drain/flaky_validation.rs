//! Accepted claimed evidence uses the same owner friction dedupe as local delivery.

use super::*;
use orbit_store::contracts::FrictionListFilter;
use orbit_store::friction_store::FrictionStore;
use orbit_types::task::TaskDecision;
use orbit_types::workflow::{FLAKY_VALIDATION_EVENT, FlakyValidation};

#[test]
fn claimed_flaky_evidence_records_history_and_reuses_local_signature() {
    if !isolated(
        module_path!(),
        "claimed_flaky_evidence_records_history_and_reuses_local_signature",
    ) {
        return;
    }
    let pair = Pair::with_owner_config(
        "[workflow]\nrequired_validation_commands = [\"test\"]\n",
        &[None],
    );
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let mut delivered = handoff(&pair.admission(&leaf));
    let owner = &pair.wire.owner;
    let context = ClaimInvocation::trusted_worker(
        delivered.task_id.clone(),
        delivered.claim_id.clone(),
        delivered.machine_id.clone(),
        Some(ClaimRun {
            machine_id: delivered.machine_id.clone(),
            run_id: delivered.run_id.clone(),
        }),
    );
    let flaky = FlakyValidation {
        command: "test".into(),
        base_sha: delivered.candidate.base.commit.clone(),
        tested_head: delivered.candidate.candidate.commit.clone(),
        run_id: delivered.run_id.clone(),
        failures: ["test suite::flaky".into()].into(),
        selection: json!({"failing_tests": ["suite::flaky"]}),
        original_exit_code: 100,
        original_output: "test suite::flaky ... FAILED".into(),
        base_output: "1 passed".into(),
        rerun_output: "1 passed".into(),
        rerun_tests_run: 1,
        validation_env: json!({}),
    };
    let log = HandoffValidationLog {
        schema_version: 1,
        workspace_id: delivered.workspace_id.clone(),
        task_id: delivered.task_id.clone(),
        claim_id: delivered.claim_id.clone(),
        machine_id: delivered.machine_id.clone(),
        run_id: delivered.run_id.clone(),
        candidate: delivered.candidate.clone(),
        tested_head: delivered.candidate.candidate.commit.clone(),
        command: flaky.command.clone(),
        exit_code: 0,
        output: flaky.rerun_output.clone(),
        known_red: None,
        flaky: Some(flaky.clone()),
    };
    let artifact = TaskArtifact {
        path: "validation/flaky.json".into(),
        content: serde_json::to_vec(&log).unwrap(),
        media_type: "application/json".into(),
        created_by: None,
    };
    delivered.validation = vec![HandoffArtifactRef {
        path: artifact.path.clone(),
        sha256: sha256_hex(&artifact.content),
    }];
    owner
        .mutate_execution_claim(
            Some(&context),
            "flaky-log",
            &ClaimMutation::Evidence(ClaimEvidence {
                artifacts: vec![artifact],
                ..Default::default()
            }),
        )
        .unwrap();
    let observation = HandoffObservation {
        candidate: delivered.candidate.clone(),
        footprint_widening: vec![],
        required_commands: vec!["test".into()],
        owner_completion_authority: None,
        review: None,
        known_red: vec![],
        light_delivery: Vec::new(),
    };
    // The local validator records this signature first. Accepted remote evidence
    // must reuse it, including a retried acceptance after a lost reply.
    let local_task = backlog_task(owner, &pair.owner_repo, "src/local.rs", None);
    RuntimeHost::record_flaky_validation(owner, &local_task, &flaky).unwrap();
    for _ in 0..2 {
        owner
            .accept_task_handoff(
                &context,
                "accept-flaky",
                delivered.clone(),
                observation.clone(),
            )
            .unwrap();
    }
    let history = owner.get_task_history(&delivered.task_id).unwrap();
    assert!(
        history
            .iter()
            .any(|entry| entry.event == FLAKY_VALIDATION_EVENT
                && entry.decision == Some(TaskDecision::FlakyValidation(flaky.clone()))
                && entry.note == Some(flaky.text()))
    );
    let store = FrictionStore::open(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
        owner.data_root().join("frictions"),
    )
    .unwrap();
    assert_eq!(
        store.list(&FrictionListFilter::default()).unwrap().len(),
        1,
        "local validation, accepted claim and acceptance retry share one flaky signature"
    );
}
