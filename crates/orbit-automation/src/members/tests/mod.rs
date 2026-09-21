use super::*;
use orbit_store::{Store, compose};
use std::{cell::RefCell, collections::BTreeSet};

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
                crew: None,
            }],
            withheld: BTreeMap::new(),
            next: None,
        })
    }

    fn admission(&self, _: &StateMember) -> Result<MemberAdmission, AutomationError> {
        Ok(self
            .deferral
            .borrow()
            .clone()
            .map(MemberAdmission::Withhold)
            .unwrap_or(MemberAdmission::Admit))
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
            Ok(MemberOutcome::Settled(MemberBatchEvidence {
                action_id: evidence.action_id.clone(),
                attempt_id: evidence.attempt_id.clone(),
                applied: vec![evidence],
                failed: BTreeMap::new(),
            }))
        } else if *self.failed.borrow() {
            Ok(MemberOutcome::Failed("fixture_failure".into()))
        } else {
            Ok(MemberOutcome::Pending)
        }
    }
}

struct SchedulerHost {
    candidates: RefCell<Vec<StateMember>>,
    withheld: RefCell<BTreeMap<String, String>>,
    retired: RefCell<BTreeSet<String>>,
    admission_checks: RefCell<Vec<String>>,
    /// Task ids of every admitted attempt, one entry per run.
    admitted: RefCell<Vec<Vec<String>>>,
    settled: RefCell<Option<MemberBatchEvidence>>,
}

impl SchedulerHost {
    fn new(candidates: Vec<StateMember>) -> Self {
        Self {
            candidates: RefCell::new(candidates),
            withheld: RefCell::new(BTreeMap::new()),
            retired: RefCell::new(BTreeSet::new()),
            admission_checks: RefCell::new(Vec::new()),
            admitted: RefCell::new(Vec::new()),
            settled: RefCell::new(None),
        }
    }
}

impl MemberHost for SchedulerHost {
    fn head(&self, _: &str) -> Result<(String, SourceRevision), AutomationError> {
        Ok(("repo".into(), source()))
    }

    fn observe(&self, _: Option<&str>, _: DateTime<Utc>) -> Result<MemberPage, AutomationError> {
        Ok(MemberPage {
            candidates: self.candidates.borrow().clone(),
            withheld: self.withheld.borrow().clone(),
            next: None,
        })
    }

    fn admission(&self, member: &StateMember) -> Result<MemberAdmission, AutomationError> {
        self.admission_checks.borrow_mut().push(member.key.clone());
        if self.retired.borrow().contains(&member.key) {
            Ok(MemberAdmission::Retire("incident_changed".into()))
        } else {
            Ok(MemberAdmission::Admit)
        }
    }

    fn lookup(&self, _: &MemberAttempt) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn admit(&self, attempt: &MemberAttempt) -> Result<String, AutomationError> {
        self.admitted.borrow_mut().push(attempt.task_ids());
        Ok(format!("run-{}", attempt.member.key))
    }

    fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
        Ok(self
            .settled
            .borrow()
            .clone()
            .map(MemberOutcome::Settled)
            .unwrap_or(MemberOutcome::Pending))
    }
}

fn source() -> SourceRevision {
    SourceRevision {
        commit: "source".into(),
        tree: "tree".into(),
    }
}

fn state_member(key: &str, task_ids: &[&str], first_seen_minute: i64) -> StateMember {
    state_member_with_crew(key, task_ids, first_seen_minute, None)
}

fn state_member_with_crew(
    key: &str,
    task_ids: &[&str],
    first_seen_minute: i64,
    crew: Option<&str>,
) -> StateMember {
    StateMember {
        key: key.into(),
        task_ids: task_ids.iter().map(|id| (*id).into()).collect(),
        fingerprint: key.into(),
        source: source(),
        evidence: serde_json::json!({}),
        first_seen: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
            + Duration::minutes(first_seen_minute),
        changed_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
            + Duration::minutes(first_seen_minute),
        crew: crew.map(str::to_string),
    }
}

fn execution_tick(
    store: &dyn AutomationStoreBackend,
    host: &SchedulerHost,
    minute: i64,
) -> AutomationDiagnostic {
    let trigger = StateTrigger {
        kind: StateTriggerKind::ExecutionFailed,
        max_items: 2,
        ..trigger()
    };

    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/recovery",
            epoch: "epoch",
            trigger: &trigger,
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
            constraints: MemberConstraints::default(),
        },
    )
    .unwrap()
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
        batch_size: None,
        eligibility: PreparationEligibility::default(),
    }
}

fn preparation_tick(
    store: &dyn AutomationStoreBackend,
    host: &SchedulerHost,
    minute: i64,
    dry_run: bool,
) -> AutomationDiagnostic {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
            constraints: MemberConstraints::default(),
        },
    )
    .unwrap()
}

fn evidence_for(attempt: &MemberAttempt, key: &str, resulting: &str) -> MemberEvidence {
    MemberEvidence {
        action_id: attempt.action_id.clone().unwrap(),
        attempt_id: attempt.id.clone(),
        member_key: key.into(),
        input_fingerprint: attempt.member_for(key).unwrap().fingerprint.clone(),
        resulting_fingerprint: resulting.into(),
        ready: true,
        result: serde_json::json!({"task_id": key}),
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
            constraints: MemberConstraints::default(),
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
fn execution_failed_retires_stale_prefix_without_losing_current_diagnostics() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member("old-incident-a", &["task-a"], 0),
        state_member("old-incident-b", &["task-b"], 0),
    ]);

    assert_eq!(
        execution_tick(store.as_ref(), &host, 0).reason,
        "debouncing"
    );

    *host.candidates.borrow_mut() = vec![state_member("new-incident", &["task-new"], 1)];
    *host.withheld.borrow_mut() = BTreeMap::from([
        ("task-a".into(), "recovery_pending".into()),
        ("task-b".into(), "human_block".into()),
    ]);
    host.retired
        .borrow_mut()
        .extend(["old-incident-a".into(), "old-incident-b".into()]);

    assert_eq!(
        execution_tick(store.as_ref(), &host, 1).reason,
        "debouncing"
    );

    let pruned = execution_tick(store.as_ref(), &host, 3);
    assert_eq!(pruned.reason, "work_withheld");
    assert_eq!(
        host.admission_checks.borrow().as_slice(),
        ["old-incident-a", "old-incident-b"]
    );
    let members = pruned.state.unwrap().members.unwrap();
    assert_eq!(
        members.pending.keys().cloned().collect::<Vec<_>>(),
        ["new-incident"]
    );
    assert_eq!(
        members.withheld,
        BTreeMap::from([
            ("task-a".into(), "recovery_pending".into()),
            ("task-b".into(), "human_block".into()),
        ])
    );

    let admitted = execution_tick(store.as_ref(), &host, 4);
    assert_eq!(admitted.reason, "fired");
    assert_eq!(
        host.admission_checks.borrow().as_slice(),
        [
            "old-incident-a",
            "old-incident-b",
            "new-incident",
            "new-incident"
        ]
    );
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [vec!["task-new".to_string()]]
    );

    *host.candidates.borrow_mut() = vec![state_member("recovered-incident", &["task-a"], 5)];
    *host.withheld.borrow_mut() = BTreeMap::from([("task-b".into(), "human_block".into())]);

    let refreshed = execution_tick(store.as_ref(), &host, 5);
    assert_eq!(refreshed.reason, "batch_pending");
    assert_eq!(
        refreshed.state.unwrap().members.unwrap().withheld,
        BTreeMap::from([("task-b".into(), "human_block".into())])
    );
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
                now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(2),
                constraints: MemberConstraints::default(),
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
                now: Utc::now(),
                constraints: MemberConstraints::default(),
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
    let yaml = "schemaVersion: 1\nname: pilot\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: machine\n    branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 30\ntarget: job:task_pilot_pipeline\n";
    assert!(parse_routine_yaml(yaml).is_ok());
    for invalid in [
        yaml.replace("trigger:\n", "trigger:\n  cron: '* * * * *'\n"),
        yaml.replace("max_items: 50", "max_items: 0"),
        yaml.replace("retries: 1", "retries: 6"),
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
        preparation::fingerprint(
            task,
            source,
            dependencies,
            instructions,
            &PreparationEligibility::default(),
        )
        .unwrap()
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

/// [ORB-12745] The resolved eligibility is material input: the default keeps
/// the `material_v1` bytes a workspace accepted before the predicate became
/// configurable, an equivalent explicit block hashes the same, and a changed
/// predicate — even one the task still satisfies — invalidates the fingerprint.
#[test]
fn material_fingerprint_folds_in_a_non_default_eligibility() {
    use orbit_types::task::{Task, TaskStatus, TaskType};
    use serde_json::json;
    let task: Task = serde_json::from_value(json!({
        "id":"ORB-00001", "title":"task", "description":"scope",
        "context_files":["file:src/lib.rs"], "status":"backlog",
        "priority":"medium", "task_type":"bug", "tags":["pilot"],
        "created_at":"2026-09-01T00:00:00Z", "updated_at":"2026-09-01T00:00:00Z"
    }))
    .unwrap();
    let hash = |eligibility: &PreparationEligibility| {
        preparation::fingerprint(&task, "source", &json!({}), "instructions", eligibility).unwrap()
    };
    let baseline = hash(&PreparationEligibility::default());
    // The default adds no key: these are the exact `material_v1` bytes the
    // contract hashed before the predicate became configurable.
    let mut expected_tags = task.tags.clone();
    expected_tags.sort();
    let pre_existing_material = json!({
        "contract": preparation::CONTRACT, "id": task.id, "title": task.title.trim(),
        "description": task.description.trim(), "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(), "selectors": task.context_files, "tags": expected_tags,
        "tools": task.required_tools, "type": task.task_type, "complexity": task.complexity,
        "crew": task.crew, "eligible": true, "relations": task.relations,
        "dependencies": json!({}), "instructions": "instructions",
        "source_revision": "source",
    });
    assert_eq!(
        baseline,
        crate::delivery::definition_epoch(&pre_existing_material).unwrap()
    );
    let spelled_out = PreparationEligibility {
        statuses: vec![TaskStatus::Backlog, TaskStatus::Proposed],
        exclude_tags: vec!["no-diff-needed".into(), "no-diff-expected".into()],
        require_tags: vec![],
        task_types: vec![],
    };
    assert_eq!(
        baseline,
        hash(&spelled_out),
        "an explicit block equal to the default is the same material"
    );

    let narrowed = PreparationEligibility {
        require_tags: vec!["pilot".into()],
        ..Default::default()
    };
    assert!(preparation::eligible(&task, &narrowed));
    assert_ne!(
        baseline,
        hash(&narrowed),
        "a changed predicate is new material"
    );
    let reordered = PreparationEligibility {
        statuses: vec![TaskStatus::Backlog, TaskStatus::Proposed],
        ..narrowed.clone()
    };
    assert_eq!(
        hash(&narrowed),
        hash(&reordered),
        "authoring order is not material"
    );

    let excluding = PreparationEligibility {
        task_types: vec![TaskType::Feature],
        ..Default::default()
    };
    assert!(!preparation::eligible(&task, &excluding));
    assert_ne!(hash(&narrowed), hash(&excluding));
}

/// [ORB-11332] A grant's resolved due interval accelerates only the members it
/// names; the operator's routine timing governs everything else.
#[test]
fn grant_scope_accelerates_only_in_scope_members() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    let evaluate_with = |minute: i64, scope: &[&str]| {
        evaluate(
            store.as_ref(),
            &host,
            MemberEvaluation {
                consumer: "host/ws/routine/pilot",
                epoch: "epoch",
                trigger: &trigger(),
                enabled: true,
                dry_run: true,
                now: DateTime::from_timestamp(1_700_000_000, 0).unwrap()
                    + Duration::minutes(minute),
                constraints: MemberConstraints {
                    scope: scope.iter().map(|id| id.to_string()).collect(),
                    due_after_seconds: Some(0),
                },
            },
        )
        .unwrap()
    };

    // Seed the member without constraints: the two-minute debounce holds.
    assert_eq!(tick(store.as_ref(), &host, 0).reason, "debouncing");
    // Out of scope: still debouncing at the same minute.
    assert_eq!(evaluate_with(0, &["other"]).reason, "debouncing");
    // In scope with a zero-second due interval: due now.
    assert_eq!(evaluate_with(0, &["task"]).reason, "would_fire");
}

/// [ORB-12746] A burst of due members is admitted as one attempt of up to
/// `batch_size` members, oldest first; the rest stay pending for the next
/// admission, and both the preview and the fired diagnostic list the batch.
#[test]
fn burst_of_due_members_admits_one_batch_and_keeps_the_rest_pending() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(
        (0..8)
            .map(|index| state_member(&format!("task-{index}"), &[&format!("task-{index}")], 0))
            .collect(),
    );

    assert_eq!(
        preparation_tick(store.as_ref(), &host, 0, false).reason,
        "debouncing"
    );

    let preview = preparation_tick(store.as_ref(), &host, 3, true);
    assert_eq!(preview.reason, "would_fire");
    assert_eq!(
        preview
            .batch
            .iter()
            .map(|member| (member.key.as_str(), member.reason.as_str()))
            .collect::<Vec<_>>(),
        (0..5)
            .map(|_| "settled")
            .enumerate()
            .map(|(i, r)| (["task-0", "task-1", "task-2", "task-3", "task-4"][i], r))
            .collect::<Vec<_>>()
    );
    assert!(
        host.admitted.borrow().is_empty(),
        "a preview admits nothing"
    );

    let fired = preparation_tick(store.as_ref(), &host, 3, false);
    assert_eq!(fired.reason, "fired");
    assert_eq!(fired.batch.len(), 5);
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [vec!["task-0", "task-1", "task-2", "task-3", "task-4"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()]
    );
    let members = fired.state.unwrap().members.unwrap();
    let active = members.active.unwrap();
    assert_eq!(active.members().len(), 5);
    assert_eq!(active.member, active.members()[0]);
    assert_eq!(active.attempt, 1);
    assert_eq!(members.pending.len(), 8, "admission alone applies nothing");

    let pending = preparation_tick(store.as_ref(), &host, 4, false);
    assert_eq!(pending.reason, "batch_pending");
    assert_eq!(pending.batch.len(), 5);
    assert!(
        pending
            .batch
            .iter()
            .all(|member| member.reason == "admitted")
    );
    assert_eq!(host.admitted.borrow().len(), 1, "one run per batch");
}

/// [ORB-12761] A mixed-crew due set is admitted as one crew-homogeneous
/// attempt; the other crew stays pending and is dispatched on the next
/// admission after the first attempt settles, so dispatch never sees a
/// mixed `task_ids` bundle.
#[test]
fn mixed_crew_due_members_dispatch_one_homogeneous_bundle_per_crew() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member_with_crew("task-0", &["task-0"], 0, Some("opus")),
        state_member_with_crew("task-1", &["task-1"], 0, Some("sol")),
        state_member_with_crew("task-2", &["task-2"], 0, Some("opus")),
        state_member("task-3", &["task-3"], 0),
    ]);

    assert_eq!(
        preparation_tick(store.as_ref(), &host, 0, false).reason,
        "debouncing"
    );

    let preview = preparation_tick(store.as_ref(), &host, 3, true);
    assert_eq!(preview.reason, "would_fire");
    assert_eq!(
        preview
            .batch
            .iter()
            .map(|member| member.key.as_str())
            .collect::<Vec<_>>(),
        ["task-0", "task-2"]
    );

    let fired = preparation_tick(store.as_ref(), &host, 3, false);
    assert_eq!(fired.reason, "fired");
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [vec!["task-0".to_string(), "task-2".to_string()]]
    );
    let attempt = fired.state.unwrap().members.unwrap().active.unwrap();
    assert_eq!(attempt.members().len(), 2);
    assert!(
        attempt
            .members()
            .iter()
            .all(|member| member.crew.as_deref() == Some("opus"))
    );

    *host.settled.borrow_mut() = Some(MemberBatchEvidence {
        action_id: attempt.action_id.clone().unwrap(),
        attempt_id: attempt.id.clone(),
        applied: vec![
            evidence_for(&attempt, "task-0", "task-0-assessed"),
            evidence_for(&attempt, "task-2", "task-2-assessed"),
        ],
        failed: BTreeMap::new(),
    });
    let assessed = |key: &str, crew: Option<&str>| StateMember {
        fingerprint: format!("{key}-assessed"),
        ..state_member_with_crew(key, &[key], 0, crew)
    };
    *host.candidates.borrow_mut() = vec![
        assessed("task-0", Some("opus")),
        state_member_with_crew("task-1", &["task-1"], 0, Some("sol")),
        assessed("task-2", Some("opus")),
        state_member("task-3", &["task-3"], 0),
    ];

    let next = preparation_tick(store.as_ref(), &host, 4, false);
    assert_eq!(next.reason, "fired");
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [
            vec!["task-0".to_string(), "task-2".to_string()],
            vec!["task-1".to_string()]
        ]
    );
    let sol_attempt = next.state.unwrap().members.unwrap().active.unwrap();
    assert_eq!(sol_attempt.task_ids(), ["task-1"]);
    assert_eq!(sol_attempt.member.crew.as_deref(), Some("sol"));
}

/// [ORB-12761] Unset `task.crew` is a distinct bundle identity: it does not
/// share an attempt with a named crew, matching the dispatch rule that
/// treats set vs unset as mixed.
#[test]
fn unset_and_named_crew_do_not_share_a_bundle() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member_with_crew("named", &["named"], 0, Some("opus")),
        state_member("unset", &["unset"], 0),
    ]);
    preparation_tick(store.as_ref(), &host, 0, false);
    let fired = preparation_tick(store.as_ref(), &host, 3, false);
    assert_eq!(fired.reason, "fired");
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [vec!["named".to_string()]]
    );
}

/// [ORB-12746] One run settles each member on its own: applied members are
/// assessed under a single receipt, a member the run did not apply is failed
/// at its fingerprint and withheld with the run's reason, and neither blocks
/// the other. A material edit to the failed member creates new work.
#[test]
fn partial_batch_outcome_records_assessed_and_failed_per_member() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member("task-a", &["task-a"], 0),
        state_member("task-b", &["task-b"], 0),
        state_member("task-c", &["task-c"], 0),
    ]);
    preparation_tick(store.as_ref(), &host, 0, false);
    let fired = preparation_tick(store.as_ref(), &host, 3, false);
    assert_eq!(fired.reason, "fired");
    let attempt = fired.state.unwrap().members.unwrap().active.unwrap();

    *host.settled.borrow_mut() = Some(MemberBatchEvidence {
        action_id: attempt.action_id.clone().unwrap(),
        attempt_id: attempt.id.clone(),
        applied: vec![
            evidence_for(&attempt, "task-a", "task-a-assessed"),
            evidence_for(&attempt, "task-c", "task-c-assessed"),
        ],
        failed: BTreeMap::from([("task-b".to_string(), "stale: task_deleted".to_string())]),
    });

    // Applying rewrote the material of a and c; b is unchanged.
    let assessed = |key: &str| StateMember {
        fingerprint: format!("{key}-assessed"),
        ..state_member(key, &[key], 0)
    };
    *host.candidates.borrow_mut() = vec![
        assessed("task-a"),
        state_member("task-b", &["task-b"], 0),
        assessed("task-c"),
    ];
    let settled = preparation_tick(store.as_ref(), &host, 4, false);
    assert_eq!(
        settled.reason, "needs_attention",
        "the failed member is visible"
    );
    let members = settled.state.unwrap().members.unwrap();
    assert!(members.active.is_none());
    assert_eq!(
        members.assessed.keys().cloned().collect::<Vec<_>>(),
        ["task-a", "task-c"]
    );
    assert!(
        members
            .assessed
            .values()
            .all(|a| a.receipt_id == attempt.id)
    );
    assert_eq!(
        members.failed.keys().cloned().collect::<Vec<_>>(),
        ["task-b"]
    );
    let failed = &members.failed["task-b"];
    assert!(failed.exhausted);
    assert_eq!(failed.id, attempt.id);
    assert_eq!(failed.member_for("task-b").unwrap().fingerprint, "task-b");
    assert_eq!(
        members.pending.keys().cloned().collect::<Vec<_>>(),
        ["task-b"],
        "applied members leave pending; the failed one waits for new material"
    );
    let receipts = store
        .automation_receipts("host/ws/routine/pilot", 20)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].batch_id, attempt.id);
    let receipt = store
        .automation_receipt("host/ws/routine/pilot", &attempt.id)
        .unwrap()
        .unwrap();
    let evidence: MemberBatchEvidence = serde_json::from_slice(&receipt.evidence).unwrap();
    assert_eq!(evidence.applied.len(), 2);
    assert_eq!(evidence.failed["task-b"], "stale: task_deleted");

    // The failed fingerprint never refires; a material edit is new work.
    *host.settled.borrow_mut() = None;
    *host.candidates.borrow_mut() = vec![state_member("task-b", &["task-b"], 0)];
    assert_eq!(
        preparation_tick(store.as_ref(), &host, 20, false).reason,
        "needs_attention"
    );
    let mut edited = state_member("task-b", &["task-b"], 21);
    edited.fingerprint = "task-b-edited".into();
    *host.candidates.borrow_mut() = vec![edited];
    // It first appeared at minute 0, so the maximum wait is already spent.
    let refired = preparation_tick(store.as_ref(), &host, 21, false);
    assert_eq!(refired.reason, "fired");
    assert_eq!(refired.batch[0].reason, "max_wait");
    assert_eq!(host.admitted.borrow().len(), 2);
    assert_eq!(host.admitted.borrow()[1], vec!["task-b".to_string()]);
}

/// [ORB-12746] A member that stopped being admissible before the batch was
/// acknowledged leaves the attempt; its siblings still fire.
#[test]
fn stale_member_leaves_an_unadmitted_batch_without_failing_siblings() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member("task-a", &["task-a"], 0),
        state_member("task-b", &["task-b"], 0),
    ]);
    preparation_tick(store.as_ref(), &host, 0, false);
    let fired = preparation_tick(store.as_ref(), &host, 3, false);
    assert_eq!(fired.reason, "fired");
    let attempt = fired.state.unwrap().members.unwrap().active.unwrap();
    assert_eq!(attempt.members().len(), 2);

    // The run stopped without any apply output: the whole attempt retries.
    *host.settled.borrow_mut() = None;
    let mut retrying = fired_state_after_failure(store.as_ref(), &host, &attempt, 4);
    assert_eq!(retrying.attempt, 2);
    assert!(retrying.action_id.is_none());

    // Before the retry is acknowledged, task-b's source identity is retired
    // and the source no longer observes it.
    host.retired.borrow_mut().insert("task-b".into());
    *host.candidates.borrow_mut() = vec![state_member("task-a", &["task-a"], 0)];
    let refired = preparation_tick(store.as_ref(), &host, 10, false);
    assert_eq!(refired.reason, "fired");
    let members = refired.state.unwrap().members.unwrap();
    let active = members.active.unwrap();
    assert_eq!(active.id, retrying.id);
    assert_eq!(
        active
            .members()
            .iter()
            .map(|m| m.key.as_str())
            .collect::<Vec<_>>(),
        ["task-a"]
    );
    assert_eq!(active.member.key, "task-a");
    assert!(
        members.failed.is_empty(),
        "a retired member is not a failure"
    );
    assert!(!members.pending.contains_key("task-b"));
    retrying.members.clear();
    assert_eq!(
        host.admitted.borrow().last().unwrap(),
        &vec!["task-a".to_string()]
    );
}

/// Drive the active attempt through one `Failed` outcome and return the
/// retrying attempt.
fn fired_state_after_failure(
    store: &dyn AutomationStoreBackend,
    host: &SchedulerHost,
    attempt: &MemberAttempt,
    minute: i64,
) -> MemberAttempt {
    struct Failing<'a>(&'a SchedulerHost);
    impl MemberHost for Failing<'_> {
        fn head(&self, b: &str) -> Result<(String, SourceRevision), AutomationError> {
            self.0.head(b)
        }
        fn observe(
            &self,
            a: Option<&str>,
            n: DateTime<Utc>,
        ) -> Result<MemberPage, AutomationError> {
            self.0.observe(a, n)
        }
        fn admission(&self, m: &StateMember) -> Result<MemberAdmission, AutomationError> {
            self.0.admission(m)
        }
        fn lookup(&self, a: &MemberAttempt) -> Result<Option<String>, AutomationError> {
            self.0.lookup(a)
        }
        fn admit(&self, a: &MemberAttempt) -> Result<String, AutomationError> {
            self.0.admit(a)
        }
        fn outcome(&self, _: &MemberAttempt) -> Result<MemberOutcome, AutomationError> {
            Ok(MemberOutcome::Failed("fixture_failure".into()))
        }
    }
    let diagnostic = evaluate(
        store,
        &Failing(host),
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled: true,
            dry_run: false,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minute),
            constraints: MemberConstraints::default(),
        },
    )
    .unwrap();
    assert_eq!(diagnostic.reason, "retry_backoff");
    let retrying = diagnostic.state.unwrap().members.unwrap().active.unwrap();
    assert_eq!(retrying.id, attempt.id);
    retrying
}

/// [ORB-12746] State persisted before batching carries `member` alone; it
/// still deserializes as a batch of one and completes through the same
/// per-member receipt path.
#[test]
fn persisted_single_member_attempt_deserializes_and_completes() {
    let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let member = state_member("task", &["task"], 0);
    let legacy: MemberAttempt = serde_json::from_value(serde_json::json!({
        "consumer": "host/ws/routine/pilot",
        "kind": "preparation_eligible",
        "id": "legacy-attempt",
        "member": member,
        "attempt": 1,
        "max_attempts": 2,
        "deadline": now + Duration::minutes(30),
        "retry_after": now,
        "action_key": "automation:legacy-attempt:1",
        "action_id": "run-legacy",
        "exhausted": false
    }))
    .unwrap();
    assert!(legacy.members.is_empty());
    assert_eq!(legacy.members(), std::slice::from_ref(&legacy.member));
    assert_eq!(legacy.task_ids(), ["task"]);
    assert!(legacy.batch_is_consistent());

    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![member.clone()]);
    let state = AutomationState {
        members: Some(MemberState::default()),
        consumer: "host/ws/routine/pilot".into(),
        epoch: "epoch".into(),
        trigger: None,
        repository: "repo".into(),
        branch: "agent-main".into(),
        generation: 0,
        baseline: source(),
        observed: source(),
        covered: source(),
        pending_commits: vec![],
        pending: vec![],
        waived: vec![],
        excluded: vec![],
        unresolved: BTreeMap::new(),
        associations: BTreeMap::new(),
        active: None,
        stall: None,
    };
    assert!(store.automation_initialize(&state).unwrap());
    // Persist the legacy claim the way the pre-batch evaluator did: claimed
    // over its pending member, then acknowledged with the run id.
    let mut claimed = state.clone();
    claimed.generation += 1;
    let members = claimed.members.as_mut().unwrap();
    members.pending.insert("task".into(), member.clone());
    members.active = Some(MemberAttempt {
        action_id: None,
        ..legacy.clone()
    });
    assert!(store.automation_commit(&state, &claimed, None).unwrap());
    let mut acknowledged = claimed.clone();
    acknowledged.generation += 1;
    acknowledged.members.as_mut().unwrap().active = Some(legacy.clone());
    assert!(
        store
            .automation_commit(&claimed, &acknowledged, None)
            .unwrap()
    );
    assert_eq!(
        preparation_tick(store.as_ref(), &host, 1, false).reason,
        "batch_pending"
    );

    *host.settled.borrow_mut() = Some(MemberBatchEvidence {
        action_id: "run-legacy".into(),
        attempt_id: "legacy-attempt".into(),
        applied: vec![evidence_for(&legacy, "task", "task-assessed")],
        failed: BTreeMap::new(),
    });
    *host.candidates.borrow_mut() = vec![StateMember {
        fingerprint: "task-assessed".into(),
        ..member
    }];
    let completed = preparation_tick(store.as_ref(), &host, 2, false);
    assert_eq!(completed.reason, "fresh");
    let members = completed.state.unwrap().members.unwrap();
    assert!(members.active.is_none());
    assert_eq!(
        members.assessed["task"].resulting_fingerprint,
        "task-assessed"
    );
    assert_eq!(
        store
            .automation_receipts("host/ws/routine/pilot", 20)
            .unwrap()[0]
            .batch_id,
        "legacy-attempt"
    );
}

/// [ORB-12746] `batch_size` parses, defaults to five capped by `max_items`,
/// and an explicit value outside `1..=min(50, max_items)` fails closed.
#[test]
fn batch_size_defaults_and_validates_against_max_items() {
    use orbit_common::protocol::yaml::parse_routine_yaml;
    let yaml = "schemaVersion: 1\nname: pilot\ntrigger:\n  state:\n    kind: preparation_eligible\n    owner_machine: machine\n    branch: agent-main\n    debounce_minutes: 2\n    max_wait_minutes: 10\n    max_items: 50\n    retries: 1\n    deadline_minutes: 30\ntarget: job:task_pilot_pipeline\n";
    let parsed = parse_routine_yaml(yaml).unwrap();
    let state = parsed.trigger.state.unwrap();
    assert_eq!(state.batch_size, None);
    assert_eq!(state.effective_batch_size(), 5);
    assert_eq!(
        StateTrigger {
            max_items: 3,
            ..trigger()
        }
        .effective_batch_size(),
        3,
        "the default never exceeds max_items"
    );
    let explicit =
        parse_routine_yaml(&yaml.replace("max_items: 50", "max_items: 50\n    batch_size: 12"))
            .unwrap()
            .trigger
            .state
            .unwrap();
    assert_eq!(explicit.batch_size, Some(12));
    assert_eq!(explicit.effective_batch_size(), 12);
    for invalid in [
        yaml.replace("max_items: 50", "max_items: 50\n    batch_size: 0"),
        yaml.replace("max_items: 50", "max_items: 50\n    batch_size: 51"),
        yaml.replace("max_items: 50", "max_items: 4\n    batch_size: 5"),
    ] {
        assert!(parse_routine_yaml(&invalid).is_err(), "{invalid}");
    }
}
