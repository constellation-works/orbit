use super::*;
use orbit_store::{Store, compose};
use std::cell::RefCell;

struct Host {
    fingerprint: RefCell<String>,
    actions: RefCell<BTreeMap<String, String>>,
    evidence: RefCell<Option<MemberEvidence>>,
    failed: RefCell<bool>,
    deferral: RefCell<Option<String>>,
    lose_ack: RefCell<bool>,
}

impl Host {
    fn new() -> Self {
        Self {
            fingerprint: RefCell::new("input".into()),
            actions: RefCell::new(BTreeMap::new()),
            evidence: RefCell::new(None),
            failed: RefCell::new(false),
            deferral: RefCell::new(None),
            lose_ack: RefCell::new(false),
        }
    }
}

impl MemberHost for Host {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok((
            "repo".into(),
            SourceRevision {
                commit: "source".into(),
                tree: "tree".into(),
            },
        ))
    }

    fn observe(&self, _: Option<&str>, now: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: vec![StateMember {
                key: "task".into(),
                task_ids: vec!["task".into()],
                fingerprint: self.fingerprint.borrow().clone(),
                source: self.head("")?.1,
                evidence: serde_json::json!({}),
                first_seen: now,
                changed_at: now,
            }],
            withheld: BTreeMap::new(),
            next: None,
        })
    }

    fn admission_deferral(&self, _: &StateMember) -> Result<Option<String>, AutomationError> {
        Ok(self.deferral.borrow().clone())
    }

    fn lookup(&self, attempt: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(self.actions.borrow().get(&attempt.action_key).cloned())
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        let id = self
            .actions
            .borrow_mut()
            .entry(attempt.action_key.clone())
            .or_insert_with(|| format!("run-{}", attempt.attempt))
            .clone();
        if *self.lose_ack.borrow() {
            return Err(AutomationError::Deferred("lost_acknowledgement".into()));
        }
        Ok(id)
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        if let Some(evidence) = self.evidence.borrow().clone() {
            Ok(MemberOutcome::Applied(evidence))
        } else if *self.failed.borrow() {
            Ok(MemberOutcome::Failed("fixture_failure".into()))
        } else {
            Ok(MemberOutcome::Pending)
        }
    }
}

fn trigger() -> StateTrigger {
    StateTrigger {
        kind: StateTriggerKind::PreparationEligible,
        owner_machine: "host".into(),
        branch: "agent-main".into(),
        debounce_minutes: 2,
        max_wait_minutes: 10,
        max_items: 50,
        retries: 1,
        deadline_minutes: 30,
    }
}

fn tick(store: &dyn AutomationStoreBackend, host: &Host, minute: i64) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
        },
    )
    .unwrap()
}

#[test]
fn fresh_unready_post_apply_does_not_loop_and_new_material_debounces() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    assert_eq!(tick(store.as_ref(), &host, 0).reason, "debouncing");
    assert_eq!(tick(store.as_ref(), &host, 1).reason, "debouncing");
    let fired = tick(store.as_ref(), &host, 2);
    assert_eq!(fired.reason, "fired");
    let attempt = fired.state.unwrap().members.unwrap().active.unwrap();
    *host.evidence.borrow_mut() = Some(MemberEvidence {
        action_id: attempt.action_id.unwrap(),
        attempt_id: attempt.id,
        member_key: "task".into(),
        input_fingerprint: "input".into(),
        resulting_fingerprint: "post-apply".into(),
        ready: false,
        result: serde_json::json!({"reason":"decision_required"}),
    });
    *host.fingerprint.borrow_mut() = "post-apply".into();
    assert_eq!(tick(store.as_ref(), &host, 3).reason, "fresh_unready");
    assert_eq!(tick(store.as_ref(), &host, 60).reason, "fresh_unready");
    assert_eq!(host.actions.borrow().len(), 1);
    assert_eq!(
        store
            .automation_receipts("host/ws/routine/pilot", 20)
            .unwrap()
            .len(),
        1
    );
    *host.fingerprint.borrow_mut() = "criteria-edit".into();
    *host.evidence.borrow_mut() = None;
    assert_eq!(tick(store.as_ref(), &host, 61).reason, "debouncing");
    assert_eq!(tick(store.as_ref(), &host, 63).reason, "fired");
}

#[test]
fn restart_preserves_attempt_budget_deadline_and_failed_input() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = compose::automation_store(Store::open(&path).unwrap()).unwrap();
    let host = Host::new();
    tick(store.as_ref(), &host, 0);
    let first = tick(store.as_ref(), &host, 2)
        .state
        .unwrap()
        .members
        .unwrap()
        .active
        .unwrap();
    *host.failed.borrow_mut() = true;
    let retry = tick(store.as_ref(), &host, 3)
        .state
        .unwrap()
        .members
        .unwrap()
        .active
        .unwrap();
    assert_eq!(retry.attempt, 2);
    assert_eq!(retry.deadline, first.deadline);
    drop(store);
    let store = compose::automation_store(Store::open(&path).unwrap()).unwrap();
    assert_eq!(tick(store.as_ref(), &host, 7).reason, "retry_backoff");
    assert_eq!(tick(store.as_ref(), &host, 8).reason, "fired");
    assert_eq!(tick(store.as_ref(), &host, 9).reason, "needs_attention");
    assert_eq!(tick(store.as_ref(), &host, 500).reason, "needs_attention");
    assert_eq!(host.actions.borrow().len(), 2);
    *host.fingerprint.borrow_mut() = "material-change".into();
    *host.failed.borrow_mut() = false;
    assert_eq!(tick(store.as_ref(), &host, 501).reason, "fired");
    assert_eq!(tick(store.as_ref(), &host, 503).reason, "batch_pending");
}

#[test]
fn crash_after_admission_recovers_key_before_new_authority_check() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    tick(store.as_ref(), &host, 0);
    *host.lose_ack.borrow_mut() = true;
    assert!(
        evaluate(
            store.as_ref(),
            &host,
            MemberEvaluation {
                consumer: "host/ws/routine/pilot",
                epoch: "epoch",
                trigger: &trigger(),
                enabled: true,
                dry_run: false,
                now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(2)
            }
        )
        .is_err()
    );
    *host.lose_ack.borrow_mut() = false;
    *host.deferral.borrow_mut() = Some("task_withdrawn".into());
    assert_eq!(tick(store.as_ref(), &host, 3).reason, "batch_pending");
    assert_eq!(host.actions.borrow().len(), 1);
}

#[test]
fn forged_member_output_never_advances_coverage() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    tick(store.as_ref(), &host, 0);
    let active = tick(store.as_ref(), &host, 2)
        .state
        .unwrap()
        .members
        .unwrap()
        .active
        .unwrap();
    *host.evidence.borrow_mut() = Some(MemberEvidence {
        action_id: "wrong-run".into(),
        attempt_id: active.id,
        member_key: "task".into(),
        input_fingerprint: "input".into(),
        resulting_fingerprint: "input".into(),
        ready: true,
        result: serde_json::json!({"ok":true}),
    });
    assert!(
        evaluate(
            store.as_ref(),
            &host,
            MemberEvaluation {
                consumer: "host/ws/routine/pilot",
                epoch: "epoch",
                trigger: &trigger(),
                enabled: true,
                dry_run: false,
                now: Utc::now()
            }
        )
        .is_err()
    );
    assert!(
        store
            .automation_receipts("host/ws/routine/pilot", 20)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn incident_identity_uses_cause_and_episode_and_requires_settled_authority() {
    use crate::members::incidents::{IncidentFacts, incident_key};
    let mut facts = IncidentFacts {
        workspace: "ws".into(),
        episode: Some("root-attempt".into()),
        cause: Some("child-step".into()),
        failure: true,
        recovery_settled: true,
        current_failure_coupling: true,
        diagnostic_origin: false,
        cancellation: false,
    };
    let key = incident_key(&facts).unwrap();
    assert_eq!(key, incident_key(&facts).unwrap());
    facts.recovery_settled = false;
    assert!(incident_key(&facts).is_err());
    facts.recovery_settled = true;
    facts.cancellation = true;
    assert!(incident_key(&facts).is_err());
    facts.cancellation = false;
    facts.diagnostic_origin = true;
    assert!(incident_key(&facts).is_err());
    facts.diagnostic_origin = false;
    facts.current_failure_coupling = false;
    assert!(incident_key(&facts).is_err());
    facts.current_failure_coupling = true;
    facts.episode = Some("new-authorized-execution".into());
    assert_ne!(key, incident_key(&facts).unwrap());
}

#[test]
fn checkpoint_rejects_budget_reset_ack_replacement_and_lost_failure_tombstone() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    tick(store.as_ref(), &host, 0);
    let state = tick(store.as_ref(), &host, 2).state.unwrap();
    for mutation in 0..3 {
        let mut next = state.clone();
        next.generation += 1;
        let active = next.members.as_mut().unwrap().active.as_mut().unwrap();
        match mutation {
            0 => active.deadline += Duration::minutes(30),
            1 => active.max_attempts += 1,
            _ => active.action_id = Some("replacement".into()),
        }
        assert!(store.automation_commit(&state, &next, None).is_err());
    }
    *host.failed.borrow_mut() = true;
    let failed = tick(store.as_ref(), &host, 40).state.unwrap();
    let mut erased = failed.clone();
    erased.generation += 1;
    erased.members.as_mut().unwrap().failed.clear();
    assert!(store.automation_commit(&failed, &erased, None).is_err());
}

#[test]
fn state_configuration_rejects_ambiguous_authority_and_invalid_budgets() {
    use orbit_common::protocol::yaml::parse_routine_yaml;
    let yaml = "schemaVersion: 1\nname: pilot\nhosts: [host]\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: machine\n    branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 30\ntarget: job:task_pilot_pipeline\n";
    assert!(parse_routine_yaml(yaml).is_ok());
    for invalid in [
        yaml.replace("trigger:\n", "trigger:\n  cron: '* * * * *'\n"),
        yaml.replace("max_items: 50", "max_items: 0"),
        yaml.replace("retries: 1", "retries: 6"),
        yaml.replace("hosts: [host]", "hosts: [host, second]"),
        yaml.replace("job:task_pilot_pipeline", "job:task_pr_pipeline"),
    ] {
        assert!(parse_routine_yaml(&invalid).is_err(), "{invalid}");
    }
}

#[test]
fn material_fingerprint_covers_contract_inputs_but_ignores_audit_writes() {
    use orbit_types::task::{Task, TaskPriority};
    use serde_json::json;
    let mut task: Task = serde_json::from_value(json!({
        "id":"ORB-00001", "title":"task", "description":"scope",
        "context_files":["file:src/lib.rs"], "status":"backlog",
        "priority":"medium", "task_type":"chore",
        "created_at":"2026-09-01T00:00:00Z", "updated_at":"2026-09-01T00:00:00Z"
    }))
    .unwrap();
    let hash = |task: &Task, source: &str, dependencies: &serde_json::Value, instructions: &str| {
        preparation::fingerprint(task, source, dependencies, instructions).unwrap()
    };
    let baseline = hash(&task, "source", &json!({}), "instructions");
    task.priority = TaskPriority::High;
    task.updated_at += Duration::minutes(1);
    task.execution_summary = "audit write".into();
    assert_eq!(baseline, hash(&task, "source", &json!({}), "instructions"));
    for field in [
        "title",
        "description",
        "plan",
        "acceptance_criteria",
        "context_files",
        "crew",
    ] {
        let mut changed = serde_json::to_value(&task).unwrap();
        changed[field] = if matches!(field, "acceptance_criteria" | "context_files") {
            json!(["changed"])
        } else {
            json!("changed")
        };
        let changed = serde_json::from_value(changed).unwrap();
        assert_ne!(
            baseline,
            hash(&changed, "source", &json!({}), "instructions"),
            "{field}"
        );
    }
    assert_ne!(
        baseline,
        hash(&task, "other-source", &json!({}), "instructions")
    );
    assert_ne!(
        baseline,
        hash(
            &task,
            "source",
            &json!({"dependency":"done"}),
            "instructions"
        )
    );
    assert_ne!(
        baseline,
        hash(&task, "source", &json!({}), "new instructions")
    );
}
