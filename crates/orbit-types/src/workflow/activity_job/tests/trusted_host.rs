use serde_json::json;

use crate::workflow::activity_job::{
    TRUSTED_HOST_ACTIVITY, TRUSTED_HOST_ADMISSION_KEY, TrustedHostAdmission,
    run_input_declares_trusted_host, strip_trusted_host_admission, validate_trusted_host_activity,
};

fn admission() -> TrustedHostAdmission {
    TrustedHostAdmission {
        authorized_by: "human".to_string(),
        authorizer_provenance: "interactive-terminal".to_string(),
        caller_machine_id: None,
        caller_identity: None,
        agent_invoke_mode: None,
        authorized_at: "2026-09-06T00:00:00Z".to_string(),
        workspace_path: "/checkout".to_string(),
        cwd: "/checkout/crates".to_string(),
    }
}

#[test]
fn only_the_builtin_activity_may_declare_trusted_host_execution() {
    assert!(validate_trusted_host_activity(TRUSTED_HOST_ACTIVITY, true).is_ok());
    let error = validate_trusted_host_activity("agent_implement", true)
        .expect_err("a second activity must not be able to claim the mode");
    assert_eq!(error.activity, "agent_implement");
    assert!(
        error.to_string().contains("admitted per invocation"),
        "the refusal must say where the mode actually comes from: {error}"
    );
}

#[test]
fn an_activity_that_does_not_declare_the_mode_is_always_accepted() {
    assert!(validate_trusted_host_activity("agent_implement", false).is_ok());
    assert!(validate_trusted_host_activity(TRUSTED_HOST_ACTIVITY, false).is_ok());
}

#[test]
fn an_admission_round_trips_through_run_input() {
    let input = json!({
        "prompt": "why",
        TRUSTED_HOST_ADMISSION_KEY: serde_json::to_value(admission()).expect("encode"),
    });
    assert!(run_input_declares_trusted_host(&input));
    assert_eq!(
        TrustedHostAdmission::from_run_input(&input).expect("decode"),
        admission()
    );
}

#[test]
fn a_malformed_admission_still_counts_as_declaring_the_reserved_key() {
    // The submission guard and the engine ask different questions on purpose:
    // a caller who supplies garbage under the reserved key is forging one and
    // must be refused, while the engine must not treat garbage as an admission.
    let input = json!({ TRUSTED_HOST_ADMISSION_KEY: "not-an-admission" });
    assert!(run_input_declares_trusted_host(&input));
    assert!(TrustedHostAdmission::from_run_input(&input).is_none());
}

#[test]
fn ordinary_run_input_declares_nothing() {
    let input = json!({ "task_ids": ["ORB-1"], "mode": "pr" });
    assert!(!run_input_declares_trusted_host(&input));
    assert!(TrustedHostAdmission::from_run_input(&input).is_none());
}

#[test]
fn stripping_reports_whether_an_admission_was_present() {
    let mut input = json!({
        "prompt": "why",
        TRUSTED_HOST_ADMISSION_KEY: serde_json::to_value(admission()).expect("encode"),
    });
    assert!(strip_trusted_host_admission(&mut input));
    assert!(!run_input_declares_trusted_host(&input));
    assert_eq!(input.get("prompt").and_then(|v| v.as_str()), Some("why"));
    assert!(!strip_trusted_host_admission(&mut input));
}
