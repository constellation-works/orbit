//! Captured review admission [ORB-11333].

use orbit_types::workflow::{REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewTiming};
use serde_json::json;

use super::{GATED_CONFIG, fixture, seed_task};
use crate::application::review::install_review_admission;

#[test]
fn delivery_submissions_capture_the_effective_policy_with_its_sources() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let mut input = json!({ "task_ids": ["ORB-1"] });

    install_review_admission(runtime, "task_pr_pipeline", &mut input, None, false)
        .expect("captured");
    let admission = ReviewAdmission::from_run_input(&input)
        .expect("readable")
        .expect("present");
    assert_eq!(admission.timing, ReviewTiming::BeforePr);
    assert_eq!(admission.timing_source, "workspace");
    assert_eq!(admission.crew.as_deref(), Some("reviewers"));
    assert_eq!(admission.budget.reviewer_starts, 2);
    assert_eq!(admission.budget.minutes, 30);
    assert_eq!(
        admission.policy_version,
        orbit_config::OPERATION_POLICY_VERSION
    );

    // Jobs outside the delivery family carry nothing.
    let mut other = json!({ "task_ids": ["ORB-1"] });
    install_review_admission(runtime, "task_pilot_pipeline", &mut other, None, false)
        .expect("ignored");
    assert!(other.get(REVIEW_ADMISSION_KEY).is_none());
}

#[test]
fn ordinary_input_cannot_supply_or_widen_the_review_admission() {
    let fixture = fixture("[operation]\nreview_policy = \"none\"\n");
    let runtime = &fixture.runtime;
    let mut forged = json!({
        "task_ids": ["ORB-1"],
        "review": { "contract_version": 1, "policy_version": 2, "timing": "none",
                    "timing_source": "forged", "crew_source": "forged",
                    "budget": { "reviewer_starts": 9, "repair_cycles": 9, "minutes": 999 },
                    "captured_at": "2026-09-07T00:00:00Z" },
    });
    let error = install_review_admission(runtime, "task_pr_pipeline", &mut forged, None, false)
        .expect_err("reserved key is refused");
    assert!(
        error.to_string().contains("reserved `review` field"),
        "{error}"
    );

    // A resume keeps whatever its persisted input carries.
    let mut resumed = forged.clone();
    install_review_admission(runtime, "task_pr_pipeline", &mut resumed, None, true)
        .expect("resume keeps persisted input");
    assert_eq!(resumed["review"]["timing_source"], "forged");
}

#[test]
fn children_inherit_their_parents_snapshot_even_after_the_preference_changes() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let task = seed_task(runtime, "child");
    let parent = super::admitted_run(
        runtime,
        "task_auto_pipeline",
        std::slice::from_ref(&task.id),
    );

    // Rewrite the workspace preference to `none`: the running lineage must
    // keep the gate it was admitted with, so a rollback cannot weaken it.
    std::fs::write(
        fixture.repo.join(".orbit/config.toml"),
        "[operation]\nreview_policy = \"none\"\n",
    )
    .expect("edit config");

    let mut child = json!({ "task_ids": [task.id] });
    install_review_admission(
        runtime,
        "task_pr_pipeline",
        &mut child,
        Some(&parent.run_id),
        false,
    )
    .expect("inherit");
    let inherited = ReviewAdmission::from_run_input(&child)
        .expect("readable")
        .expect("present");
    let captured = ReviewAdmission::from_run_input(parent.input.as_ref().expect("parent input"))
        .expect("readable")
        .expect("present");
    assert_eq!(inherited, captured);
    assert_eq!(inherited.timing, ReviewTiming::BeforePr);
}

#[test]
fn before_pr_is_refused_on_the_local_only_route() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let mut input = json!({ "task_ids": ["ORB-1"] });
    let error = install_review_admission(runtime, "task_local_pipeline", &mut input, None, false)
        .expect_err("local route cannot honour before-pr");
    assert!(
        error
            .to_string()
            .contains("no meaning on the local-only delivery route"),
        "{error}"
    );

    let after_landing = fixture;
    std::fs::write(
        after_landing.repo.join(".orbit/config.toml"),
        "[operation]\nreview_policy = \"after-landing\"\n",
    )
    .expect("edit config");
    let (_root, runtime, _) =
        crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config(Some(
            "[operation]\nreview_policy = \"after-landing\"\n",
        ));
    let mut local = json!({ "task_ids": ["ORB-1"] });
    install_review_admission(&runtime, "task_local_pipeline", &mut local, None, false)
        .expect("after-landing is fine locally");
    assert_eq!(local["review"]["timing"], "after-landing");
}
