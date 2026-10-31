use super::*;
use orbit_store::contracts::V2AuditEventInsertParams;
use orbit_types::workflow::{
    ExperimentOverlayRecord, TagInstructionRole, V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
};

/// Records `kind` in `run`'s audit trail.
fn audit(runtime: &OrbitRuntime, run: &str, event_id: &str, kind: V2AuditEventKind) {
    let event = V2AuditEvent {
        envelope: V2AuditEnvelope {
            schema_version: 1,
            event_type: "fixture".into(),
            event_id: event_id.into(),
            ts: Utc::now(),
            run_id: run.into(),
            agent_identity: "claude".into(),
            parent_event_id: None,
            workspace_path: None,
        },
        kind,
    };
    runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: runtime.workspace_id().unwrap(),
            event_id: event_id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "fixture".into(),
            ts: event.envelope.ts,
            run_id: run.into(),
            agent_identity: "claude".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: serde_json::to_string(&event).unwrap(),
        })
        .unwrap();
}

/// A started, expired experiment whose `treatment` arm overlays the
/// implement instructions, with settled implement tokens that put the
/// treatment's primary interval clearly below control's. Each treatment
/// task's implement step receives the overlay and then finishes with
/// `step_outcome`.
fn expired_instruction_experiment(root: &std::path::Path, step_outcome: &str) -> ExperimentReadout {
    let global = root.join("home/.orbit");
    let workspace = root.join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[experiments]\nenabled = true\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "treatment-delivery",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    {
                        "name": "treatment",
                        "overlay": { "instructions": { "implement": "docs/agents/treatment.md" } }
                    }
                ])),
                primary_metric: Some("implement_tokens".into()),
                min_sample: Some(100),
                max_days: Some(1),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    let workspace_id = runtime.workspace_id().unwrap();
    let store = runtime.sqlite_store().unwrap();
    let mut record = store
        .experiment_record(&workspace_id, &experiment.id)
        .unwrap()
        .unwrap();
    let revision = record.experiment.revision;
    record.experiment.status = "running".into();
    record.experiment.segment = 1;
    record.experiment.revision += 1;
    assert_eq!(
        store
            .write_experiment(&ExperimentWrite {
                event: ExperimentEventRow {
                    seq: record.experiment.revision,
                    kind: "started".into(),
                    from_status: Some("draft".into()),
                    to_status: Some("running".into()),
                    segment: 1,
                    actor: actor.actor,
                    at: Utc::now() - Duration::days(2),
                    reason: None,
                    detail_json: json!({"segment": 1, "weights": {"control": 1, "treatment": 1}})
                        .to_string(),
                },
                experiment: record.experiment,
                arms: None,
                expected_revision: Some(revision),
                running_limit: None,
            })
            .unwrap(),
        ExperimentWriteOutcome::Written
    );
    for tokens in [200, 210, 190, 205] {
        enroll_primary_sample(
            &runtime,
            &experiment.id,
            "control",
            TaskComplexity::Low,
            Some(tokens),
        );
    }
    for (index, tokens) in [100, 110, 90, 105].into_iter().enumerate() {
        let task = enroll_primary_sample(
            &runtime,
            &experiment.id,
            "treatment",
            TaskComplexity::Low,
            Some(tokens),
        );
        let run = format!("run-{index}");
        audit(
            &runtime,
            &run,
            &format!("overlay-{index}"),
            V2AuditEventKind::ExperimentOverlayApplied {
                step_id: "implement".into(),
                activity_name: "agent_implement".into(),
                task_id: task,
                overlays: vec![ExperimentOverlayRecord {
                    experiment: experiment.id.clone(),
                    arm: "treatment".into(),
                    segment: 1,
                    role: TagInstructionRole::Implement,
                    instruction: None,
                    suppressed_default_instructions: false,
                    tools_added: Vec::new(),
                    tools_removed: Vec::new(),
                    skills: Default::default(),
                    digest: "sha256:treatment".into(),
                }],
            },
        );
        audit(
            &runtime,
            &run,
            &format!("finished-{index}"),
            V2AuditEventKind::StepFinished {
                step_id: "implement".into(),
                lock_wait_ms: None,
                outcome: step_outcome.into(),
                error_message: None,
            },
        );
    }
    runtime.experiment_readout(&experiment.id, None).unwrap()
}

/// The exp-2b6d910d shape: the treatment arm's overlaid implement step
/// fails on every assigned run, so the arm got control's work, yet the
/// primary interval excludes zero. The readout must withhold the verdict
/// rather than crown a treatment that never ran.
#[test]
fn readout_withholds_the_verdict_when_the_treatment_never_ran() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::delivery::readout_withholds_the_verdict_when_the_treatment_never_ran",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let readout = expired_instruction_experiment(root.path(), "error");
    let primary = readout
        .metrics
        .iter()
        .find(|metric| metric.metric == "implement_tokens")
        .unwrap();
    let [_, high] = primary.differences[0].overall.interval.unwrap();
    assert!(high < 0.0, "the primary interval excludes zero");
    assert_eq!(readout.verdict.state, "withheld", "{:?}", readout.verdict);
    assert_eq!(
        readout.verdict.reasons,
        ["arm `treatment` treatment delivered on 0/4 runs"]
    );
    let arm = readout
        .arms
        .iter()
        .find(|arm| arm.name == "treatment")
        .unwrap();
    let delivery = arm.delivery.as_ref().unwrap();
    assert_eq!(
        (delivery.assigned, delivery.delivered, delivery.rate),
        (4, 0, Some(0.0))
    );
    assert!(
        readout.arms[0].delivery.is_none(),
        "control has no delivery check"
    );
    let per_protocol = readout.per_protocol.as_ref().unwrap();
    assert_eq!(per_protocol.label, "per-protocol (delivered runs only)");
    assert_eq!(per_protocol.metric.arms[1].estimate.n, 0);
    assert!(readout.warnings.iter().any(|w| w.kind == "undelivered"));
}

/// When the overlaid step completes on every run, delivery changes nothing:
/// the verdict is today's winner, and the per-protocol view matches the
/// intention-to-treat one.
#[test]
fn readout_keeps_the_verdict_when_the_treatment_is_delivered() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::delivery::readout_keeps_the_verdict_when_the_treatment_is_delivered",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let readout = expired_instruction_experiment(root.path(), "success");
    assert_eq!(readout.verdict.state, "winner", "{:?}", readout.verdict);
    assert_eq!(readout.verdict.arm.as_deref(), Some("treatment"));
    let arm = readout
        .arms
        .iter()
        .find(|arm| arm.name == "treatment")
        .unwrap();
    let delivery = arm.delivery.as_ref().unwrap();
    assert_eq!(
        (delivery.assigned, delivery.delivered, delivery.rate),
        (4, 4, Some(1.0))
    );
    let per_protocol = readout.per_protocol.as_ref().unwrap();
    assert_eq!(per_protocol.metric.arms[1].estimate.n, 4);
    assert!(!readout.warnings.iter().any(|w| w.kind == "undelivered"));
}
