use super::*;

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
            }
        )
        .is_err()
    );
    *host.lose_ack.borrow_mut() = false;
    *host.deferral.borrow_mut() = Some("task_withdrawn".into());
    assert_eq!(tick(store.as_ref(), &host, 3).reason, "batch_pending");
    assert_eq!(host.actions.borrow().len(), 1);
}
