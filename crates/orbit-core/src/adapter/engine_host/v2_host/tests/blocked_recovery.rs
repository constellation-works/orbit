//! The apply step reads the agent's decision out of a step output that also
//! carries Orbit's invocation metadata. The decision parser rejects unknown
//! fields, so passing the merged object through would escalate every valid
//! decision; this pins the projection that keeps the two apart.

use orbit_types::workflow::FinalRecoveryDecision;
use serde_json::json;

use super::super::blocked_recovery::decision_result;

#[test]
fn only_the_envelope_result_reaches_the_decision_parser() {
    let output = json!({
        "decision": "archive",
        "reason": "superseded by a later task",
        "response_result_fields": ["decision", "reason"],
        "response_envelope_valid": true,
        "final_message": "done",
        "provider": "codex",
        "exit_code": 0,
    });
    let decision = decision_result(&output).expect("a valid envelope projects");
    assert_eq!(
        FinalRecoveryDecision::parse(&decision),
        Ok(FinalRecoveryDecision::Archive {
            reason: "superseded by a later task".to_string(),
        })
    );

    let invalid = json!({
        "decision": "archive",
        "reason": "superseded",
        "response_result_fields": null,
        "response_envelope_valid": false,
    });
    assert_eq!(
        decision_result(&invalid),
        None,
        "an invalid envelope is no decision, which the applier escalates"
    );
}
