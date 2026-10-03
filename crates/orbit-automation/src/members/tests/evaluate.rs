use super::*;

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

/// [ORB-12796] The crew-homogeneity filter is not specific to
/// `preparation_eligible`: two due `execution_failed` members carrying
/// different stored `task.crew` never fire in the same attempt either.
#[test]
fn execution_failed_mixed_crew_members_do_not_share_a_bundle() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = SchedulerHost::new(vec![
        state_member_with_crew("task-0", &["task-0"], 0, Some("opus")),
        state_member_with_crew("task-1", &["task-1"], 0, Some("sol")),
    ]);

    assert_eq!(
        execution_tick(store.as_ref(), &host, 0).reason,
        "debouncing"
    );

    let fired = execution_tick(store.as_ref(), &host, 3);
    assert_eq!(fired.reason, "fired");
    assert_eq!(
        host.admitted.borrow().as_slice(),
        [vec!["task-0".to_string()]],
        "the sol-crewed member must wait rather than share this bundle"
    );
    let attempt = fired.state.unwrap().members.unwrap().active.unwrap();
    assert_eq!(attempt.members().len(), 1);
    assert_eq!(attempt.member.crew.as_deref(), Some("opus"));
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
