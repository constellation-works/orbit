//! `delivery_stale_branch` has two reachable holds with different remedies: a
//! retained candidate branch is inspected, an owner base checkout is
//! preserved and reconciled with origin. The two shipped copies of the hold
//! guide must agree, and the typed next step must name both remedies. The
//! guide's prose is not pinned here; the runtime refusal fixtures cover each
//! hold's behavior.

use orbit_types::workflow::DeliveryRefusal;

const CORE_GUIDE: &str = include_str!(
    "../../../../orbit-core/assets/skills/orbit-orchestrate/references/run-debugging-delivery-holds.md"
);
const PLUGIN_GUIDE: &str = include_str!(
    "../../../../../plugin/skills/orbit-orchestrate/references/run-debugging-delivery-holds.md"
);

#[test]
fn shipped_guide_copies_agree() {
    assert_eq!(CORE_GUIDE, PLUGIN_GUIDE);
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

#[test]
fn unattached_base_recovery_needs_no_checkout() {
    let step = DeliveryRefusal::StaleBranch.operator_next_step("jrun-1");
    for fragment in [
        "no checkout holds",
        "temporary worktree",
        "merge origin/<base>",
        "do not delete the branch",
    ] {
        assert!(
            step.contains(fragment),
            "next step must say `{fragment}`: {step}"
        );
    }
}
