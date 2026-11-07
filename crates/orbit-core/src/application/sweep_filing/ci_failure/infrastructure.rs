//! Failures whose log carries a declared infrastructure signature
//! (`workflow.ci_infrastructure_signatures`): reported, not filed.

use orbit_engine::ci_log_signature::declared_infrastructure_signature;
use serde_json::{Value, json};

/// Separate the failures a declared signature names. Each keeps only its
/// identity and the matched signature: the log has no repair to brief.
pub(super) fn split_declared_infrastructure(
    failures: Vec<Value>,
    signatures: &[String],
) -> (Vec<Value>, Vec<Value>) {
    let mut remaining = Vec::new();
    let mut infrastructure = Vec::new();
    for failure in failures {
        let matched = failure
            .get("log_excerpt")
            .and_then(Value::as_str)
            .and_then(|log| declared_infrastructure_signature(log, signatures));
        match matched {
            Some(signature) => infrastructure.push(json!({
                "run_id": failure.get("run_id"),
                "job_id": failure.get("job_id"),
                "workflow": failure.get("workflow"),
                "url": failure.get("url"),
                "signature": signature,
            })),
            None => remaining.push(failure),
        }
    }
    (remaining, infrastructure)
}

pub(super) fn infrastructure_audit(mut audit: Value, infrastructure: &[Value]) -> Value {
    audit["infrastructure"] = json!(infrastructure.len());
    audit
}
