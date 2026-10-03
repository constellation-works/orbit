use crate::application::task::contention::{LockSurface, compute_contention};

fn surface(task_id: &str, selectors: &[&str]) -> LockSurface {
    LockSurface {
        task_id: task_id.to_string(),
        selectors: selectors.iter().map(|s| (*s).to_string()).collect(),
    }
}

#[test]
fn contention_follows_selector_containment_not_string_equality() {
    // `dir:src` covers everything beneath it, so these two tasks conflict even
    // though neither declares the other's selector.
    let report = compute_contention(&[
        surface("T-1", &["dir:src"]),
        surface("T-2", &["file:src/nested/deep.rs"]),
    ]);

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
