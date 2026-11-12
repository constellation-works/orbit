//! A `friction_reported` decision obeys the common decision text bound on
//! every friction field it supplies, through every parser entry point.

use orbit_types::workflow::{
    FinalRecoveryDecision, FinalRecoveryError, MAX_DECISION_TEXT_CHARS, RecoveryFault,
};
use serde_json::{Value, json};

fn friction(fields: Value) -> Value {
    let mut output = json!({
        "decision": "friction_reported",
        "human_action": "Fix the gate, then requeue the task.",
        "fault": "orbit",
        "fault_evidence": "run jrun-1 step validate",
    });
    if let (Value::Object(output), Value::Object(fields)) = (&mut output, fields) {
        output.extend(fields);
    }
    output
}

/// [ORB-24053] An 8,001-character friction field was accepted although the
/// decision text cap is 8,000; each field is now refused by name, and the
/// applier's `from_output` escalates instead of filing it.
#[test]
fn friction_reported_fields_obey_the_decision_text_cap() {
    let at_cap = "x".repeat(MAX_DECISION_TEXT_CHARS);
    let over_cap = "x".repeat(MAX_DECISION_TEXT_CHARS + 1);
    let cases = [
        (
            "friction_body",
            json!({ "friction_title": "Gate misreads output", "friction_body": over_cap }),
        ),
        (
            "friction_title",
            json!({ "friction_title": over_cap, "friction_body": "The gate misreads output." }),
        ),
        ("friction_id", json!({ "friction_id": over_cap })),
    ];
    for (field, fields) in cases {
        let output = friction(fields);
        assert_eq!(
            FinalRecoveryDecision::parse_attributed(&output),
            Err(FinalRecoveryError::FieldTooLong { field }),
            "parse_attributed must refuse an oversized {field}"
        );
        assert_eq!(
            FinalRecoveryDecision::parse(&output),
            Err(FinalRecoveryError::FieldTooLong { field }),
            "parse must refuse an oversized {field}"
        );
        assert_eq!(
            FinalRecoveryDecision::from_output(Some(&output)).kind(),
            "escalate",
            "the applier must not file an oversized {field}"
        );
        let (decision, fault) = FinalRecoveryDecision::from_attributed_output(Some(&output));
        assert_eq!(decision.kind(), "escalate", "{field}");
        assert_eq!(fault.fault, RecoveryFault::Unknown, "{field}");
    }

    let valid = [
        json!({ "friction_title": at_cap, "friction_body": at_cap }),
        json!({ "friction_id": "F2026-11-001" }),
        json!({
            "friction_title": "Gate misreads output",
            "friction_body": "The gate misreads output.",
            "summary": "The validation gate misread its output.",
        }),
    ];
    for fields in valid {
        let output = friction(fields);
        let (decision, fault) = FinalRecoveryDecision::parse_attributed(&output)
            .unwrap_or_else(|error| panic!("{output} must parse: {error}"));
        assert_eq!(decision.kind(), "friction_reported");
        assert_eq!(fault.fault, RecoveryFault::Orbit);
    }
}
