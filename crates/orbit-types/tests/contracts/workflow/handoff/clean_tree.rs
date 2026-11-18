use orbit_types::workflow::handoff::AlreadyResolved;

#[test]
fn already_resolved_refuses_more_than_the_selector_cap() {
    let selectors = (0..=AlreadyResolved::MAX_SELECTORS)
        .map(|i| format!("cargo test -p orbit-types --test contracts case_{i}"))
        .collect::<Vec<_>>();
    let output = serde_json::json!({"already_resolved": {"selectors": selectors}});
    let mut declared = AlreadyResolved::from_output(&output).unwrap().unwrap();
    let error = declared
        .admit()
        .expect_err("host replay must bound agent-controlled test executions");
    assert!(error.contains("at most"), "{error}");
    declared.selectors.pop();
    assert_eq!(
        declared.admit().unwrap().len(),
        AlreadyResolved::MAX_SELECTORS
    );
}
