#[test]
fn contention_follows_selector_containment_not_string_equality() {
    if !super::isolate(
        module_path!(),
        "contention_follows_selector_containment_not_string_equality",
    ) {
        return;
    }

    // `dir:src` covers everything beneath it, so these two tasks conflict even
    // though neither declares the other's selector.
    let (_root, runtime, repo) = super::tool_support::test_runtime();
    std::fs::create_dir_all(repo.join("src/nested")).unwrap();
    std::fs::write(repo.join("src/nested/deep.rs"), "fixture").unwrap();
    for selectors in [&["dir:src"][..], &["file:src/nested/deep.rs"][..]] {
        super::tool_support::create_context_task(
            &runtime,
            &repo,
            orbit_types::task::TaskStatus::Backlog,
            selectors,
        );
    }
    let report = runtime
        .task_lock_contention(&[orbit_types::task::TaskStatus::Backlog])
        .unwrap();

    assert_eq!(
        report.groups, 1,
        "a directory claim reaches its descendants"
    );
    assert_eq!(report.largest_group, 2);
    assert_eq!(
        report.hotspots.len(),
        2,
        "both selectors are contended: each overlaps a surface held by the other task"
    );
}
