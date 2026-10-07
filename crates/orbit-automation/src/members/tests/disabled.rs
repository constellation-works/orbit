use super::*;

fn evaluate_with(
    store: &dyn AutomationStoreBackend,
    host: &Host,
    enabled: bool,
    dry_run: bool,
) -> Result<AutomationDiagnostic, AutomationError> {
    evaluate(
        store,
        host,
        MemberEvaluation {
            consumer: "host/ws/routine/pilot",
            epoch: "epoch",
            trigger: &trigger(),
            enabled,
            dry_run,
            now: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        },
    )
}

#[test]
fn disabled_consumer_without_state_never_probes_the_branch_in_either_mode() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    *host.head_probes.borrow_mut() = Some(0);

    for dry_run in [false, true] {
        let diagnostic = evaluate_with(store.as_ref(), &host, false, dry_run).unwrap();
        assert_eq!(diagnostic.reason, "disabled", "dry_run={dry_run}");
        assert!(diagnostic.state.is_none(), "dry_run={dry_run}");
        assert_eq!(*host.head_probes.borrow(), Some(0), "dry_run={dry_run}");
        assert!(
            store
                .automation_state("host/ws/routine/pilot")
                .unwrap()
                .is_none(),
            "dry_run={dry_run} must not create state"
        );
    }
}

#[test]
fn enabled_consumer_without_state_still_probes_the_branch() {
    let store = compose::automation_store(Store::open_in_memory().unwrap()).unwrap();
    let host = Host::new();
    *host.head_probes.borrow_mut() = Some(0);

    for dry_run in [false, true] {
        assert!(evaluate_with(store.as_ref(), &host, true, dry_run).is_err());
    }
    assert_eq!(*host.head_probes.borrow(), Some(2));
}
