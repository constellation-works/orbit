use super::*;

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

/// Without a receipt a checkpoint may retire an assessment from working
/// state, but never add or rewrite one.
#[test]
fn checkpoint_retires_but_never_forges_assessments() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = RetentionHost::new(["task".to_string()]);
    let mut minute = 0;
    assess_source(store.as_ref(), &host, &mut minute);
    let state = store
        .automation_state("host/ws/routine/pilot")
        .unwrap()
        .unwrap();
    let assessment = state.members.as_ref().unwrap().assessed["task"].clone();

    let forged = |key: &str, resulting: &str| {
        let mut next = state.clone();
        next.generation += 1;
        next.members.as_mut().unwrap().assessed.insert(
            key.into(),
            MemberAssessment {
                resulting_fingerprint: resulting.into(),
                ..assessment.clone()
            },
        );
        next
    };
    assert!(
        store
            .automation_commit(&state, &forged("other", "task"), None)
            .is_err()
    );
    assert!(
        store
            .automation_commit(&state, &forged("task", "rewritten"), None)
            .is_err()
    );

    let mut retired = state.clone();
    retired.generation += 1;
    retired.members.as_mut().unwrap().assessed.clear();
    assert!(store.automation_commit(&state, &retired, None).unwrap());
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
        fn observable(&self, k: &BTreeSet<String>) -> Result<BTreeSet<String>, AutomationError> {
            self.0.observable(k)
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
        ..member.clone()
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

    // [ORB-13638] A new fingerprint contract re-hashes the unchanged task:
    // its assessment stays fresh exactly while the host vouches that the
    // earlier contract's certificate still holds.
    *host.candidates.borrow_mut() = vec![StateMember {
        fingerprint: "task-material-v2".into(),
        ..member
    }];
    host.carried.borrow_mut().insert("task".into());
    assert_eq!(
        preparation_tick(store.as_ref(), &host, 3, false).reason,
        "fresh"
    );
    host.carried.borrow_mut().clear();
    assert_eq!(
        preparation_tick(store.as_ref(), &host, 4, false).reason,
        "fired",
        "an assessment the host cannot vouch for is assessed again"
    );
}
