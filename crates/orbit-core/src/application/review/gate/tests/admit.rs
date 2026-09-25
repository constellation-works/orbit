//! Admitting a reviewer: crew resolution, the captured snapshot, the admitted run.

use std::fs;

use chrono::Utc;
use serde_json::json;

use crate::application::review::review_gate_admit;
use crate::application::review::tests::{GATED_CONFIG, admit_input};
use crate::application::task::TaskUpdateParams;

use super::support::gated_fixture;

#[test]
fn reviewer_crew_must_be_configured_resolvable_and_allowed() {
    let unconfigured = gated_fixture(
        "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\n",
    );
    let error = unconfigured.admit().expect_err("no crew");
    assert!(
        error.to_string().contains("review_crew_unconfigured"),
        "{error}"
    );

    let missing = gated_fixture(
        "[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"ghosts\"\n",
    );
    let error = missing.admit().expect_err("unknown crew");
    assert!(
        error.to_string().contains("review_crew_unavailable"),
        "{error}"
    );

    // The run's crew allowlist excludes the reviewer: escalate, never substitute.
    let excluded = gated_fixture(GATED_CONFIG);
    let mut input = admit_input(
        &excluded.run_id,
        std::slice::from_ref(&excluded.task_id),
        &excluded.fixture.repo,
    );
    let run = excluded
        .fixture
        .runtime
        .get_job_run_backend(&excluded.run_id)
        .expect("run")
        .expect("present");
    let mut run_input = run.input.clone().expect("input");
    run_input["allowed_crews"] = json!(["implementer"]);
    // Persist the narrower allowlist as the drain would have captured it.
    let narrowed = orbit_engine::RuntimeHost::insert_job_run(
        &excluded.fixture.runtime,
        "task_pr_pipeline",
        1,
        Utc::now(),
        Some(run_input),
        None,
    )
    .expect("insert narrowed run");
    excluded
        .fixture
        .runtime
        .update_task(
            &excluded.task_id,
            TaskUpdateParams {
                job_run_id: Some(Some(narrowed.run_id.clone())),
                ..TaskUpdateParams::default()
            },
        )
        .expect("rebind");
    input["job_run_id"] = json!(narrowed.run_id);
    let error = review_gate_admit(&excluded.fixture.runtime, "review_gate_admit", &input)
        .expect_err("excluded crew");
    assert!(
        error.to_string().contains("review_crew_excluded"),
        "{error}"
    );
}

#[test]
fn the_gate_reads_the_captured_snapshot_not_the_live_preference() {
    // Captured `before-pr`, then the workspace rolls back to `none`: the
    // active gate still applies.
    let gated = gated_fixture(GATED_CONFIG);
    fs::write(
        gated.fixture.repo.join(".orbit/config.toml"),
        "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[operation]\nreview_policy = \"none\"\n",
    )
    .expect("roll back");
    let admission = gated.admit().expect("admit");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["timing_source"], "workspace");

    // Captured `none`, later switched to `before-pr`: no retroactive gate.
    let ungated = gated_fixture("[operation]\nreview_policy = \"none\"\n");
    let admission = ungated.admit().expect("admit");
    assert_eq!(admission["applies"], false);
    assert_eq!(admission["reason"], "review_policy_none");
    let settled = ungated.settle(&admission).expect("not required");
    assert_eq!(settled["gate"], "not_required");
    assert_eq!(settled["reviewed_head_sha"], "");

    // A checked no-diff exemption never creates coverage.
    let no_diff = gated_fixture(GATED_CONFIG);
    let mut input = admit_input(
        &no_diff.run_id,
        std::slice::from_ref(&no_diff.task_id),
        &no_diff.fixture.repo,
    );
    input["skipped_no_diff_expected"] = json!(true);
    let admission =
        review_gate_admit(&no_diff.fixture.runtime, "review_gate_admit", &input).expect("exempt");
    assert_eq!(admission["applies"], false);
    assert_eq!(admission["reason"], "no_diff_exemption");

    // A local route under a before-pr admission refuses rather than skipping.
    let mut local = admit_input(
        &no_diff.run_id,
        std::slice::from_ref(&no_diff.task_id),
        &no_diff.fixture.repo,
    );
    local["mode"] = json!("local");
    let error = review_gate_admit(&no_diff.fixture.runtime, "review_gate_admit", &local)
        .expect_err("local route");
    assert!(
        error
            .to_string()
            .contains("review_policy_local_route_refused"),
        "{error}"
    );
}

#[test]
fn a_worktree_token_does_not_mask_the_admitted_run() {
    let gated = gated_fixture(GATED_CONFIG);
    let worktree_token = format!("worktree-{}", gated.task_id);
    let mut input = admit_input(
        &worktree_token,
        std::slice::from_ref(&gated.task_id),
        &gated.fixture.repo,
    );

    let missing = review_gate_admit(&gated.fixture.runtime, "review_gate_admit", &input)
        .expect("a worktree token is not a run record");
    assert_eq!(missing["applies"], false);
    assert_eq!(missing["reason"], "review_admission_missing");

    input["run_id"] = json!(&gated.run_id);
    let admission = review_gate_admit(&gated.fixture.runtime, "review_gate_admit", &input)
        .expect("the injected admitted run still applies");
    assert_eq!(admission["applies"], true);
    assert_eq!(admission["decision"], "admitted");
    assert_eq!(admission["reviewer"]["crew"], "reviewers");
}
