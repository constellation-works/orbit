//! Failures whose log carries a declared infrastructure signature
//! (`workflow.ci_infrastructure_signatures`): reported, not filed.

use orbit_engine::ci_log_signature::declared_infrastructure_signature;
use orbit_tools::github_cli::strip_ansi_sequences;
use serde_json::{Value, json};

use super::evidence::selected_diagnostic;
use super::fields::value_string;

/// Separate the failures a declared signature names. Each keeps only its
/// identity and the matched signature: the log has no repair to brief.
///
/// A waiter whose only diagnostic is a `dependency_failed` record naming
/// jobs that are themselves infrastructure failures takes their class: it
/// is folded into them and filed nowhere. A record naming any job that is
/// not an infrastructure failure in this snapshot, or a malformed record,
/// keeps the waiter's ordinary filing route.
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
            None => remaining.push((failure, log)),
        }
    }
    // Fold dependency waiters into the infrastructure jobs they name.
    let infra_by_job: std::collections::BTreeMap<u64, String> = infrastructure
        .iter()
        .filter_map(|entry| {
            Some((
                entry.get("job_id")?.as_u64()?,
                entry.get("signature")?.as_str()?.to_string(),
            ))
        })
        .collect();
    let mut folded = Vec::new();
    let mut kept = Vec::new();
    for (failure, log) in remaining {
        match dependency_fold_signature(&failure, &log, &infra_by_job) {
            Some((signature, dependency_ids)) => folded.push(json!({
                "run_id": failure.get("run_id"),
                "job_id": failure.get("job_id"),
                "workflow": failure.get("workflow"),
                "url": failure.get("url"),
                "signature": signature,
                "dependency_job_ids": dependency_ids,
            })),
            None => kept.push(failure),
        }
    }
    infrastructure.extend(folded);
    (kept, infrastructure)
}

/// The infrastructure signature a waiter folds into, when its log's only
/// diagnostic is a well-formed `dependency_failed` record and every job it
/// names is an infrastructure failure in this snapshot.
fn dependency_fold_signature(
    failure: &Value,
    log: &str,
    infra_by_job: &std::collections::BTreeMap<u64, String>,
) -> Option<(String, Vec<u64>)> {
    let mut record: Option<Value> = None;
    for line in log.lines() {
        // Sweep logs carry job/step columns; the payload follows them.
        let payload = line
            .rsplit('\t')
            .next()
            .unwrap_or(line)
            .trim_start_matches(|c: char| c.is_ascii_digit() || "TZ:-. ".contains(c));
        let Some((_, body)) = payload.split_once("orbit-dependency-evidence:") else {
            // Any other non-empty diagnostic line is the waiter's own.
            let trimmed = payload.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with("ci-await")
                && !trimmed.starts_with("##[")
            {
                return None;
            }
            continue;
        };
        if record.is_some() {
            return None;
        }
        record = Some(serde_json::from_str(body.trim()).ok()?);
    }
    let record = record?;
    if record.get("kind").and_then(Value::as_str) != Some("dependency_failed")
        || record.get("schema_version").and_then(Value::as_u64) != Some(1)
    {
        return None;
    }
    if let (Some(record_run), Some(failure_run)) = (
        record.get("run_id").and_then(Value::as_u64),
        failure.get("run_id").and_then(Value::as_u64),
    ) && record_run != failure_run
    {
        return None;
    }
    let dependencies = record.get("dependencies")?.as_array()?;
    if dependencies.is_empty() {
        return None;
    }
    let mut ids = Vec::new();
    let mut signature: Option<String> = None;
    for dependency in dependencies {
        let id = dependency.get("job_id")?.as_u64()?;
        let matched = infra_by_job.get(&id)?;
        signature.get_or_insert_with(|| matched.clone());
        ids.push(id);
    }
    signature.map(|signature| (signature, ids))
}

pub(super) fn infrastructure_audit(mut audit: Value, infrastructure: &[Value]) -> Value {
    audit["infrastructure"] = json!(infrastructure.len());
    audit
}
