//! Captured review admission [ORB-11333].

use orbit_types::workflow::{REVIEW_ADMISSION_KEY, ReviewAdmission, ReviewTiming};
use serde_json::json;

use super::{GATED_CONFIG, admit_input, fixture, implement_candidate, seed_task};
use crate::application::job::pipeline::{
    ChildPipelineAdmission, ChildSubmission, worker_command_override,
};
use crate::application::review::{install_review_admission, review_gate_admit};

/// Replaces the detached pipeline worker so child submission can persist a
/// run without re-executing the test binary.
struct WorkerOverride;

impl WorkerOverride {
    fn install() -> Self {
        worker_command_override::set(["sh", "-c", "sleep 1"]);
        Self
    }
}

impl Drop for WorkerOverride {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}

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

#[test]
fn epic_parent_assembles_a_local_child_under_before_pr() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let epic = seed_task(runtime, "epic");
    let parent = super::admitted_run(runtime, "epic_pipeline", std::slice::from_ref(&epic.id));

    let mut child = json!({ "task_ids": ["ORB-CHILD"] });
    install_review_admission(
        runtime,
        "task_local_pipeline",
        &mut child,
        Some(&parent.run_id),
        false,
    )
    .expect("epic assembly is not local-only final delivery");
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
fn non_epic_parent_cannot_assemble_local_child_under_before_pr() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let leaf = seed_task(runtime, "leaf");
    let parent = super::admitted_run(
        runtime,
        "task_auto_pipeline",
        std::slice::from_ref(&leaf.id),
    );

    let mut child = json!({ "task_ids": [leaf.id] });
    let error = install_review_admission(
        runtime,
        "task_local_pipeline",
        &mut child,
        Some(&parent.run_id),
        false,
    )
    .expect_err("ordinary local-only child still refused");
    assert!(
        error
            .to_string()
            .contains("no meaning on the local-only delivery route"),
        "{error}"
    );
}

#[test]
fn caller_shaped_input_cannot_claim_an_epic_assembly_exemption() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let mut forged = json!({
        "task_ids": ["ORB-1"],
        "parent_run_id": "jrun-forged-epic",
        "job_name": "epic_pipeline",
        "epic_assembly": true,
    });
    let error = install_review_admission(runtime, "task_local_pipeline", &mut forged, None, false)
        .expect_err("ordinary input cannot claim the exemption");
    assert!(
        error
            .to_string()
            .contains("no meaning on the local-only delivery route"),
        "{error}"
    );
}

#[test]
fn pr_bound_epic_submits_a_local_child_then_gates_the_combined_candidate() {
    let fixture = fixture(GATED_CONFIG);
    let runtime = &fixture.runtime;
    let epic = seed_task(runtime, "epic");
    let child = seed_task(runtime, "child");
    let parent = super::admitted_run(runtime, "epic_pipeline", std::slice::from_ref(&epic.id));
    runtime
        .seed_v2_pipeline_run(
            &parent,
            parent.input.as_ref().expect("parent input"),
            None,
            orbit_types::workflow::JobRunTrigger::cli(),
        )
        .expect("parent pipeline state");
    implement_candidate(&fixture.repo, &epic.id);

    let _worker = WorkerOverride::install();
    let submission = runtime
        .submit_child_pipeline_run(
            "task_local_pipeline",
            json!({
                "task_ids": [child.id],
                "base_branch": "epic/branch",
                "base_sync": "local",
                "auto_push": false,
                "landing_branch": "main",
                "terminal_status": "done",
            }),
            None,
            Some("tester"),
            &ChildPipelineAdmission {
                parent_run_id: parent.run_id.clone(),
                parent_step_id: Some("land_child".to_string()),
                action: "invoke_and_wait".to_string(),
                blocking: true,
            },
        )
        .expect("epic child local pipeline is admitted");
    let ChildSubmission::Submitted(result) = submission else {
        panic!("expected a submitted child, got {submission:?}");
    };
    let child_run = runtime.show_job_run(&result.run_id).expect("child run");
    let inherited = ReviewAdmission::from_run_input(child_run.input.as_ref().expect("child input"))
        .expect("readable")
        .expect("present");
    assert_eq!(inherited.timing, ReviewTiming::BeforePr);
    assert_eq!(child_run.job_id, "task_local_pipeline");

    let admission = review_gate_admit(
        runtime,
        "review_gate_admit",
        &admit_input(
            &parent.run_id,
            std::slice::from_ref(&epic.id),
            &fixture.repo,
        ),
    )
    .expect("epic gate admits the combined candidate");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["reviewer"]["crew"], "reviewers");
}
