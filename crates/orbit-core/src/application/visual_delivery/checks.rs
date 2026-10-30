//! `visual_checks`: the deterministic checks (§7) over the capture of
//! record. The workspace's UI rules ran on each capture's live page inside
//! the driver; this step reads their results and adds the capture checks
//! (`render_timeout`, `setup_failed`, `image_over_budget`, `off_loopback_request`,
//! `color_scheme_leak`, `unstable_view`). In this phase every finding is
//! recorded in `visual/checks.json`; nothing blocks and nothing is filed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{
    VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VisualCaptureOutcome, VisualCaptureRecord,
    VisualCheckFinding, VisualChecksRecord, visual_review_summary,
};
use serde_json::{Value, json};

use super::{attach_system_artifacts, json_artifact, read_record};
use crate::OrbitRuntime;
use crate::application::visual_capture::VisualCaptureMock;

pub(crate) fn visual_checks_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let captures = input
        .get("capture")
        .and_then(|capture| capture.get("captures"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut results = Vec::new();
    for capture in &captures {
        let result = check_task(runtime, capture)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        results.push(result);
    }
    Ok(json!({
        "applies": !results.is_empty(),
        "blocking": false,
        "checks": results,
    }))
}

fn check_task(runtime: &OrbitRuntime, capture: &Value) -> Result<Value, OrbitError> {
    let task_id = capture
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| OrbitError::InvalidInput("capture entry names no task_id".into()))?;
    let (record, _) =
        read_record::<VisualCaptureRecord>(runtime, task_id, VISUAL_CAPTURE_ARTIFACT)?.ok_or_else(
            || OrbitError::Execution(format!("{task_id} has no {VISUAL_CAPTURE_ARTIFACT}")),
        )?;
    let scratch = capture
        .get("scratch_dir")
        .and_then(Value::as_str)
        .map(Path::new);
    let (rule_findings, rule_results, rules_unavailable) =
        if record.outcome == VisualCaptureOutcome::Captured {
            rule_findings(runtime, scratch)
        } else {
            (
                Vec::new(),
                BTreeMap::new(),
                Some("the capture is not established".to_string()),
            )
        };
    let mut findings = capture_findings(&record);
    let image_budget_mocks: BTreeSet<&str> = record
        .mocks
        .iter()
        .filter(|mock| {
            mock.error
                .as_ref()
                .is_some_and(|error| error.code == "image_over_budget")
        })
        .map(|mock| mock.artifact.as_str())
        .collect();
    if !image_budget_mocks.is_empty()
        && let Some((earlier_checks, _)) =
            read_record::<VisualChecksRecord>(runtime, task_id, VISUAL_CHECKS_ARTIFACT)?
        && earlier_checks.candidate_sha == record.candidate_sha
    {
        findings.extend(earlier_checks.findings.into_iter().filter(|finding| {
            matches!(
                finding.check.as_str(),
                "check:off_loopback_request" | "check:color_scheme_leak" | "check:unstable_view"
            ) && finding
                .mock
                .as_deref()
                .is_some_and(|mock| image_budget_mocks.contains(mock))
        }));
    }
    findings.extend(rule_findings);
    let checks = VisualChecksRecord {
        schema_version: 1,
        mode: record.mode,
        candidate_sha: record.candidate_sha.clone(),
        blocking: false,
        findings,
        rule_results,
        rules_unavailable,
    };
    attach_system_artifacts(
        runtime,
        task_id,
        vec![json_artifact(VISUAL_CHECKS_ARTIFACT, &checks)?],
    )?;
    Ok(json!({
        "task_id": task_id,
        "findings": checks.findings.len(),
        "checks_artifact": VISUAL_CHECKS_ARTIFACT,
        "summary": visual_review_summary(&record, Some(&checks), None),
    }))
}

/// The capture checks of §7, from what the driver recorded per mock.
fn capture_findings(record: &VisualCaptureRecord) -> Vec<VisualCheckFinding> {
    let mut findings = Vec::new();
    for mock in &record.mocks {
        if let Some(error) = &mock.error
            && matches!(
                error.code.as_str(),
                "render_timeout" | "setup_failed" | "image_over_budget"
            )
        {
            let (severity, title, against) = match error.code.as_str() {
                "setup_failed" => (
                    "high",
                    "The app's setup failed before the view rendered",
                    "change",
                ),
                "image_over_budget" => (
                    "medium",
                    "The captured images exceeded the artifact size limit",
                    "change",
                ),
                _ => ("high", "The view did not reach its ready state", "change"),
            };
            findings.push(finding(
                &error.code,
                &mock.artifact,
                severity,
                title,
                &error.message,
                against,
                None,
            ));
        }
        let Some(capture) = &mock.capture else {
            continue;
        };
        if !capture.off_loopback_requests.is_empty() {
            findings.push(finding(
                "off_loopback_request",
                &mock.artifact,
                "medium",
                "The view requested an address off loopback",
                &capture.off_loopback_requests.join(", "),
                "change",
                None,
            ));
        }
        if capture.color_scheme_leak == Some(true) {
            findings.push(finding(
                "color_scheme_leak",
                &mock.artifact,
                "medium",
                "The view rendered differently under the opposite colour preference",
                "the app declares `color_scheme_invariant`, but its capture changed when the \
                 browser preferred the other scheme",
                "change",
                None,
            ));
        }
        if capture.stability == "unstable" {
            findings.push(finding(
                "unstable_view",
                &mock.artifact,
                "low",
                "The view rendered differently on repeated captures",
                "three captures in fresh contexts did not agree; the view's fixture, mask or \
                 ready selector needs fixing in the view manifest",
                "manifest",
                capture.diff_region,
            ));
        }
    }
    findings
}

/// Findings from completed checks on driver mocks that could not attach images.
pub(super) fn image_budget_diagnostic_findings(
    mocks: &[VisualCaptureMock],
) -> Vec<VisualCheckFinding> {
    let mut findings = Vec::new();
    for mock in mocks {
        if !mock
            .error
            .as_ref()
            .is_some_and(|error| error.code == "image_over_budget")
        {
            continue;
        }
        if let Some(requests) = mock.off_loopback_requests.as_ref()
            && !requests.is_empty()
        {
            findings.push(finding(
                "off_loopback_request",
                &mock.artifact,
                "medium",
                "The view requested an address off loopback",
                &requests.join(", "),
                "change",
                None,
            ));
        }
        if mock.color_scheme_leak == Some(true) {
            findings.push(finding(
                "color_scheme_leak",
                &mock.artifact,
                "medium",
                "The view rendered differently under the opposite colour preference",
                "the app declares `color_scheme_invariant`, but its capture changed when the \
                 browser preferred the other scheme",
                "change",
                None,
            ));
        }
        if mock.stability.as_deref() == Some("unstable") {
            findings.push(finding(
                "unstable_view",
                &mock.artifact,
                "low",
                "The view rendered differently on repeated captures",
                "three captures in fresh contexts did not agree; the view's fixture, mask or \
                 ready selector needs fixing in the view manifest",
                "manifest",
                mock.diff_region,
            ));
        }
    }
    findings
}

fn finding(
    check: &str,
    mock: &str,
    severity: &str,
    title: &str,
    detail: &str,
    against: &str,
    region: Option<[u32; 4]>,
) -> VisualCheckFinding {
    VisualCheckFinding {
        check: format!("check:{check}"),
        mock: Some(mock.to_string()),
        severity: severity.to_string(),
        title: title.to_string(),
        detail: detail.to_string(),
        against: against.to_string(),
        region,
        measurements: None,
    }
}

type RuleOutcome = (
    Vec<VisualCheckFinding>,
    BTreeMap<String, u64>,
    Option<String>,
);

/// The workspace UI rules' failed results from the driver's `rules.json`,
/// less `ux_rules.disabled`, with every result's status counted.
fn rule_findings(runtime: &OrbitRuntime, scratch: Option<&Path>) -> RuleOutcome {
    let Some(path) = scratch.map(|scratch| scratch.join("out").join("rules.json")) else {
        return (
            Vec::new(),
            BTreeMap::new(),
            Some("the capture step named no scratch directory".to_string()),
        );
    };
    let document: Value = match std::fs::read(&path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
    {
        Ok(document) => document,
        Err(error) => {
            return (
                Vec::new(),
                BTreeMap::new(),
                Some(format!("rule results are unreadable: {error}")),
            );
        }
    };
    if document.get("source").is_none_or(Value::is_null) {
        return (
            Vec::new(),
            BTreeMap::new(),
            Some("the candidate has no `scripts/ux-rules/ui` rules".to_string()),
        );
    }
    let disabled = runtime.ux_rules_disabled();
    let mut counts = BTreeMap::new();
    let mut findings = Vec::new();
    for result in document
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let rule = result.get("rule").and_then(Value::as_str).unwrap_or("");
        if rule.is_empty() || disabled.iter().any(|id| id == rule) {
            continue;
        }
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("error");
        *counts.entry(status.to_string()).or_insert(0) += 1;
        if status != "fail" {
            continue;
        }
        findings.push(VisualCheckFinding {
            check: format!("rule:{rule}"),
            mock: result
                .get("mock")
                .and_then(Value::as_str)
                .map(str::to_string),
            severity: result
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("medium")
                .to_string(),
            title: result
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(rule)
                .to_string(),
            detail: result
                .get("selector")
                .and_then(Value::as_str)
                .filter(|selector| !selector.is_empty())
                .map(|selector| format!("first failing element: {selector}"))
                .unwrap_or_else(|| "see measurements".to_string()),
            against: "change".to_string(),
            region: None,
            measurements: result.get("measurements").cloned(),
        });
    }
    (findings, counts, None)
}

impl OrbitRuntime {
    /// `ux_rules.disabled`: rule ids whose results are recorded only.
    fn ux_rules_disabled(&self) -> Vec<String> {
        orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            self.global_root(),
            self.shared_root(),
        ))
        .ok()
        .and_then(|config| {
            config
                .values()
                .iter()
                .find(|entry| entry.key == "ux_rules.disabled")
                .map(|entry| entry.value.clone())
        })
        .and_then(|value| value.as_array().cloned())
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
    }
}
