#![allow(missing_docs)]

//! [ORB-11325] A `when:` / `break_when:` condition may only read the output
//! of a step that always runs.

use super::*;

fn step_with_when(id: &str, when: &str, action: &str) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        when: Some(when.to_string()),
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        body: JobV2StepBody::Target(deterministic_target(action)),
    }
}

#[test]
fn validate_job_rejects_when_reading_output_of_a_conditionally_run_step() {
    let conditional = step_with_when("maybe_run", "{{ input.flag }} == true", "maybe_run_action");
    let reader = step_with_when(
        "reader",
        "{{ steps.maybe_run.output.done }} == true",
        "reader_action",
    );

    let err = validate_job(&job_with_steps(vec![conditional, reader]))
        .expect_err("reading a conditional step's output must be rejected");

    match &err {
        DispatchError::JobValidation(message) => {
            assert!(
                message.contains("reader"),
                "message must name the reading step: {message}"
            );
            assert!(
                message.contains("maybe_run"),
                "message must name the conditional step: {message}"
            );
        }
        other => panic!("expected JobValidation, got {other:?}"),
    }
}
