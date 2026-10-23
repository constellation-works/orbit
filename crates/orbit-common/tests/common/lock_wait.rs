//  Synchronous scope attribution must survive nesting, threads and unwinding.

#[test]
fn nested_scopes_threads_and_unwinding_do_not_leak_waits() {
    let (_, outer) = measure(|| {
        record(Duration::from_millis(7));
        let (_, inner) = measure(|| record(Duration::from_millis(11)));
        assert_eq!(inner, 11);
        std::thread::spawn(|| record(Duration::from_secs(1)))
            .join()
            .unwrap();
        let panic = std::panic::catch_unwind(|| {
            measure(|| {
                record(Duration::from_secs(1));
                panic!("injected unwind");
            })
        });
        assert!(panic.is_err());
        record(Duration::from_millis(5));
    });
    assert_eq!(outer, 12);
    record(Duration::from_secs(1));
    assert_eq!(measure(|| {}).1, 0);
}
