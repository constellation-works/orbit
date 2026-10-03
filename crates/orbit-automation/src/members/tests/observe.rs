use super::*;

/// Assessments of members that left the source no longer hold the consumer
/// at capacity: a newly eligible member is observed and admitted, working
/// state stays bounded, and every receipt stays durable.
#[test]
fn departed_assessments_make_room_for_a_newly_eligible_member() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = RetentionHost::new((0..1000).map(|i| format!("m-{i:04}")));
    let mut minute = 0;
    assess_source(store.as_ref(), &host, &mut minute);
    let history = host.admitted.borrow().clone();
    assert_eq!(
        history
            .iter()
            .map(|attempt| attempt.members().len())
            .sum::<usize>(),
        1000
    );

    // Sorting after every retained key, the new member is on the next page.
    *host.source.borrow_mut() = BTreeMap::from([("new".to_string(), "new".to_string())]);
    minute += 3;
    let observed = retention_tick(store.as_ref(), &host, minute);
    assert_eq!(observed.reason, "debouncing");
    let members = retained_members(&observed);
    assert!(
        members.assessed.is_empty(),
        "departed assessments leave working state"
    );
    assert_eq!(members.pending.keys().collect::<Vec<_>>(), ["new"]);
    assert_eq!(*host.observable_queries.borrow(), 1);

    minute += 3;
    assert_eq!(
        retention_tick(store.as_ref(), &host, minute).reason,
        "fired"
    );
    assert_eq!(host.admitted.borrow().last().unwrap().task_ids(), ["new"]);
    for attempt in &history {
        assert!(
            store
                .automation_receipt("host/ws/routine/pilot", &attempt.id)
                .unwrap()
                .is_some(),
            "retiring an assessment keeps its receipt"
        );
    }
}

/// At capacity with every retained member still observed, the scan keeps
/// cycling, a retained member's fresh fingerprint is still assessed,
/// unchanged members never refire, and a new member waits visibly for room
/// that opens once another member leaves the source.
#[test]
fn a_full_working_set_keeps_scanning_and_admitting_retained_members() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = RetentionHost::new((0..1000).map(|i| format!("m-{i:04}")));
    let mut minute = 0;
    assess_source(store.as_ref(), &host, &mut minute);
    let assessed = host.admitted.borrow().len();

    // Both share the first page: one retained member edited, one new member.
    host.source
        .borrow_mut()
        .insert("m-0001".into(), "m-0001-edited".into());
    host.source
        .borrow_mut()
        .insert("m-0000+".into(), "m-0000+".into());

    let mut cursors = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for _ in 0..30 {
        minute += 3;
        let diagnostic = retention_tick(store.as_ref(), &host, minute);
        let members = retained_members(&diagnostic);
        assert!(members.pending.len() + members.assessed.len() + members.withheld.len() <= 1000);
        assert!(!members.pending.contains_key("m-0000+"));
        cursors.insert(members.scan_after);
        reasons.insert(diagnostic.reason);
    }
    assert!(
        cursors.contains(&None) && cursors.len() > 21,
        "the scan keeps advancing and wraps around"
    );
    assert!(reasons.contains("source_backpressure"));
    {
        let admitted = host.admitted.borrow();
        let later = &admitted[assessed..];
        assert_eq!(later.len(), 1, "only the edited member refires");
        assert_eq!(later[0].task_ids(), ["m-0001"]);
        assert_eq!(later[0].member.fingerprint, "m-0001-edited");
    }

    let departed = host
        .admitted
        .borrow()
        .iter()
        .find(|attempt| attempt.member_for("m-0999").is_some())
        .unwrap()
        .id
        .clone();
    host.source.borrow_mut().remove("m-0999");
    for _ in 0..30 {
        minute += 3;
        retention_tick(store.as_ref(), &host, minute);
        if host.admitted.borrow().len() > assessed + 1 {
            break;
        }
    }
    assert_eq!(
        host.admitted.borrow().last().unwrap().task_ids(),
        ["m-0000+"]
    );
    let members = retained_members(&retention_tick(store.as_ref(), &host, minute + 3));
    assert!(!members.assessed.contains_key("m-0999"));
    assert!(
        store
            .automation_receipt("host/ws/routine/pilot", &departed)
            .unwrap()
            .is_some()
    );
}
