//! `delivery_stale_branch` has two reachable holds with different remedies: a
//! retained candidate branch is inspected, an owner base checkout is
//! preserved and reconciled with origin. The typed next step and both shipped
//! copies of the hold guide must carry both, and the copies must agree.

use orbit_types::workflow::DeliveryRefusal;

const CORE_GUIDE: &str = include_str!(
    "../../../../orbit-core/assets/skills/orbit-orchestrate/references/run-debugging-delivery-holds.md"
);
const PLUGIN_GUIDE: &str = include_str!(
    "../../../../../plugin/skills/orbit-orchestrate/references/run-debugging-delivery-holds.md"
);

fn stale_branch_row(guide: &str) -> &str {
    guide
        .lines()
        .find(|line| line.starts_with("| `delivery_stale_branch`"))
        .expect("the guide has a delivery_stale_branch row")
}

#[test]
fn shipped_guide_copies_agree() {
    assert_eq!(CORE_GUIDE, PLUGIN_GUIDE);
}

#[test]
fn guide_separates_the_owner_base_remedy_from_the_retained_branch_remedy() {
    for guide in [CORE_GUIDE, PLUGIN_GUIDE] {
        let row = stale_branch_row(guide);
        for fragment in [
            "refusing stale branch",
            "move the branch aside or delete it",
            "refusing stale base",
            "best-effort",
            "do not move aside or delete anything",
            "git merge --ff-only origin/<base>",
        ] {
            assert!(row.contains(fragment), "the row must say `{fragment}`");
        }
    }
}

#[test]
fn typed_next_step_names_both_remedies() {
    let step = DeliveryRefusal::StaleBranch.operator_next_step("jrun-1");
    for fragment in [
        "retained branch",
        "move the branch aside",
        "owner base",
        "reconcile",
    ] {
        assert!(
            step.contains(fragment),
            "next step must say `{fragment}`: {step}"
        );
    }
}
