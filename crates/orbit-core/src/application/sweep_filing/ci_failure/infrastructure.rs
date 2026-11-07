//! Failures whose log carries a declared infrastructure signature
//! (`workflow.ci_infrastructure_signatures`): reported, not filed.

use orbit_engine::ci_log_signature::declared_infrastructure_signature;
use orbit_tools::github_cli::strip_ansi_sequences;
use serde_json::{Value, json};

use super::evidence::selected_diagnostic;
use super::fields::value_string;

/// Separate the failures a declared signature names. Each keeps only its
/// identity and the matched signature: the log has no repair to brief.
pub(super) fn split_declared_infrastructure(
    failures: Vec<Value>,
    signatures: &[String],
) -> (Vec<Value>, Vec<Value>) {
    let mut remaining = Vec::new();
    let mut infrastructure = Vec::new();
    for failure in failures {
        // Prefer the validated complete diagnostic (same provenance as
        // clustering) so a truncated display cannot hide a declared
        // signature; fall back to the bounded display otherwise.
        let raw = selected_diagnostic(&failure)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value_string(&failure, "log_excerpt"));
        let log = strip_ansi_sequences(&raw);
        let matched = declared_infrastructure_signature(&log, signatures);
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
