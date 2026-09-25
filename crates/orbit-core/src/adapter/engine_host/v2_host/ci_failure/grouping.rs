//! Grouping current failures by root cause, and the identities a cluster keys on.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::cluster::{FailureCluster, job_log_source};
use super::evidence::{selected_diagnostic, valid_failure_regions};
use super::fields::value_string;
use super::log_signature::{
    classify_log_lines, compiler_cause, error_signature, is_libtest_stdout_header,
    legacy_signature, signature_payload,
};
use crate::adapter::engine_host::v2_host::admission::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint,
};
use crate::adapter::engine_host::v2_host::admission::sweep_filing::digest;

/// Group current failures by root cause.
///
/// Preserves the order collection chose (integration head first, then release,
/// then pull requests), so the filing cap spends itself on the heads that gate
/// delivery.
pub(super) fn cluster_failures(failures: &[Value]) -> Vec<FailureCluster> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, FailureCluster> = BTreeMap::new();

    for failure in failures {
        // A listed-but-uninvestigated failure carries no job, step, or log —
        // only a run URL. Filing a task from it would produce a task whose
        // evidence section is empty, which is worse than reporting it as an
        // unfiled bound. `truncation` already names how many there were.
        if failure.get("investigated").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let workflow = value_string(failure, "workflow");
        let (job, step) = failing_job_and_step(failure);
        let log_excerpt = selected_diagnostic(failure)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value_string(failure, "log_excerpt"));
        let signature = error_signature(&log_excerpt, &step);
        let test_names = failure_test_names(failure);
        let test_identity = test_names.join("\u{1f}");
        let grouping_identity = if test_identity.is_empty() {
            format!("signature:{}", signature.text)
        } else {
            format!("test:{test_identity}")
        };
        let tested_commit = tested_commit(failure);

        let regions = valid_failure_regions(&failure["diagnostic_unit"]);
        // Partial command retention cannot prove an exhaustive compiler set.
        let compiler_cause = (!regions).then(|| compiler_cause(&log_excerpt)).flatten();
        let legacy_key = compiler_cause.as_ref().map(|_| {
            let lines = classify_log_lines(&log_excerpt);
            let legacy = legacy_signature(&lines, &step);
            digest(&[&workflow, &job, &step, &legacy])
        });
        // Cross-job consolidation requires the complete compiler diagnostic
        // set, exact source locations and the same observed checkout. Generic
        // step wrappers and shared paths are never sufficient.
        let failure_key = match &compiler_cause {
            Some(cause) => digest(&["compiler", cause, &tested_commit]),
            None => digest(&[&workflow, &job, &step, &signature.text]),
        };
        let cluster_key = if compiler_cause.is_some() {
            digest(&[&failure_key, &tested_commit])
        } else {
            digest(&[&workflow, &step, &grouping_identity, &tested_commit])
        };

        let cluster = grouped.entry(cluster_key.clone()).or_insert_with(|| {
            order.push(cluster_key.clone());
            FailureCluster {
                failure_key,
                cluster_key: cluster_key.clone(),
                workflow,
                job: job.clone(),
                jobs: BTreeSet::new(),
                step,
                tested_commit,
                signature: signature.text,
                compiler_cause,
                legacy_keys: BTreeSet::new(),
                signature_is_step_fallback: signature.step_fallback,
                log_excerpt,
                failure_region_note: regions.then(|| {
                    let unit = &failure["diagnostic_unit"];
                    format!(
                        "_Failure regions from a completely scanned command; the full command was not retained. {} of {} source bytes omitted, including {} assertion payload bytes. All {} recognized failure anchors and bounded context are retained (64 KiB selection limit)._\n\n",
                        unit["omitted_bytes"], unit["command_bytes"],
                        unit["assertion_payload_omitted_bytes"], unit["failure_anchor_count"],
                    )
                }),
                log_truncated: failure
                    .get("log_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                log_source_job: job_log_source(failure),
                runs: Vec::new(),
            }
        });
        if !job.is_empty() {
            cluster.jobs.insert(job);
        }
        if let Some(key) = legacy_key {
            cluster.legacy_keys.insert(key);
        }
        cluster.runs.push(failure.clone());
    }

    order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect()
}

/// The single job and step whose evidence passed registration.
fn failing_job_and_step(failure: &Value) -> (String, String) {
    let Some(job) = failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .and_then(|jobs| jobs.first())
    else {
        return (String::new(), String::new());
    };
    let step = job
        .get("failed_steps")
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
        .map(|step| value_string(step, "name"))
        .unwrap_or_default();
    (value_string(job, "name"), step)
}

/// The observed checkout, validated before clustering. Event and PR heads
/// are never substitutes for the commit the supplying job tested.
pub(super) fn tested_commit(failure: &Value) -> String {
    failure
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .and_then(|shas| shas.first())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default()
}

/// Extract stable test identities from the diagnostic or an explicit
/// collector field. These are intentionally separate from the normalized
/// error signature: a manual task may name the test without copying the CI
/// wrapper labels or the exact diagnostic wording.
pub(super) fn failure_test_names(failure: &Value) -> Vec<String> {
    let mut names = BTreeSet::new();
    for key in ["test_name", "failing_test", "failed_test"] {
        if let Some(name) = failure.get(key).and_then(Value::as_str)
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
    }
    for key in ["test_names", "failing_tests", "failed_tests"] {
        if let Some(values) = failure.get(key).and_then(Value::as_array) {
            names.extend(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned),
            );
        }
    }

    let log = {
        let excerpt = value_string(failure, "log_excerpt");
        if excerpt.is_empty() {
            value_string(&failure["diagnostic_unit"], "text")
        } else {
            excerpt
        }
    };
    let mut after_failures_header = false;
    for line in log.lines() {
        let payload = signature_payload(line);
        let line = payload.trim().to_string();
        if line == "failures:" || line == "errors:" {
            after_failures_header = true;
            continue;
        }
        if is_libtest_stdout_header(&line) || line.starts_with("test result:") {
            after_failures_header = false;
        }
        if let Some(rest) = line.strip_prefix("test ")
            && let Some((name, suffix)) = rest.split_once(" ... ")
            && matches!(suffix.split_whitespace().next(), Some("failed"))
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("thread '")
            && let Some((name, suffix)) = rest.split_once("' panicked")
            && !name.trim().is_empty()
            && !suffix.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("thread \"")
            && let Some((name, suffix)) = rest.split_once("\" panicked")
            && !name.trim().is_empty()
            && !suffix.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("fail [")
            && let Some((_, name)) = rest.split_once(']')
            && !name.trim().is_empty()
        {
            names.insert(name.trim().to_string());
        }
        if after_failures_header
            && (payload.starts_with(' ') || payload.starts_with('\t'))
            && !line.contains(' ')
            && !line.starts_with("----")
            && !line.starts_with("thread")
            && !line.starts_with("error")
            && !line.starts_with("assertion")
        {
            names.insert(line.trim().to_string());
        }
    }
    names.into_iter().collect()
}

/// Generated descriptions shipped these exact provenance labels. This is a
/// compatibility check for old tags, not a free-text root-cause heuristic.
pub(super) fn legacy_source_matches(description: &str, run: &Value) -> bool {
    let run_id = value_string(run, "run_id");
    let job_id = value_string(run, "job_id");
    let checkout = tested_commit(run);
    !run_id.is_empty()
        && !job_id.is_empty()
        && !checkout.is_empty()
        && description.contains(&format!("run `{run_id}`"))
        && description.contains(&format!("(id `{job_id}`)"))
        && description.contains(&format!("commit actually checked out: `{checkout}`"))
}

pub(super) fn source_identity_fingerprint(runs: &[Value]) -> Option<CoverageFingerprint> {
    let run = runs.first()?;
    let run_id = value_string(run, "run_id");
    let job_id = value_string(run, "job_id");
    if run_id.is_empty() || job_id.is_empty() {
        return None;
    }
    Some(CoverageFingerprint::new(
        "ci_failure_source_identity",
        vec![
            CoverageAnchor::new("run_id", run_id),
            CoverageAnchor::new("job_id", job_id),
        ],
    ))
}
