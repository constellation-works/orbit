use chrono::{TimeZone, Utc};

use crate::workflow::{RunIdRole, run_id_candidate, run_id_minute_stem, run_id_role};

#[test]
fn minted_ids_declare_their_role() {
    let stem = run_id_minute_stem(
        Utc.with_ymd_and_hms(2026, 9, 11, 1, 46, 12)
            .single()
            .expect("submission instant"),
    );
    assert_eq!(stem, "jrun-20260911-0146");

    let sibling = run_id_candidate(&stem, RunIdRole::TopLevel, 2);
    let child = run_id_candidate(&stem, RunIdRole::Child, 2);

    assert_eq!(sibling, "jrun-20260911-0146-t2");
    assert_eq!(child, "jrun-20260911-0146-c2");
    assert_ne!(sibling, child);
    assert_eq!(run_id_role(&sibling), Some(RunIdRole::TopLevel));
    assert_eq!(run_id_role(&child), Some(RunIdRole::Child));
}

/// Ids minted before role markers existed say nothing about their role, and
/// saying so is the point: guessing would relabel an old child as top-level.
#[test]
fn ids_without_a_role_marker_report_no_role() {
    for legacy in [
        "jrun-20260911-0146",
        "jrun-20260911-0146-2",
        "jrun-20260911-0146-ci_failure_sweep_pipeline",
        "jrun-20260911-0146-t",
        "jrun-20260911-0146-x1",
        "run-20260911-0146-t1",
    ] {
        assert_eq!(run_id_role(legacy), None, "unexpected role for {legacy}");
    }
}

#[test]
fn role_renders_an_operator_facing_label() {
    assert_eq!(RunIdRole::TopLevel.to_string(), "top-level");
    assert_eq!(RunIdRole::Child.to_string(), "child");
}
