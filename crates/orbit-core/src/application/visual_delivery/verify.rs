//! `visual_verify_prepare`: turn the comparison reviewer's findings and
//! the deterministic check findings into verifier partitions (§11), and
//! the escalation round's partitions (§11.2). Evidence is validated
//! here, before any verifier runs: a finding naming an image outside
//! this delivery's capture, a region outside its image, or a cited
//! line that does not exist is dropped as `invalid_evidence` and
//! counted, as finding verification drops a verdict citing a missing
//! line. Findings past `max_findings` are dropped and counted too.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, create_private_dir_all};
use orbit_engine::DispatchError;
use orbit_types::task::{
    VISUAL_VERIFY_BATCH_SIZE, VisualEvidence, VisualImageCatalog, VisualVerdictKind,
    parse_visual_region_location, validate_visual_evidence, validate_visual_region_location,
};
use serde_json::{Value, json};

use super::required_string;
use crate::OrbitRuntime;

const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "nit"];

pub(crate) fn visual_verify_prepare_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace = PathBuf::from(required_string(input, action, "workspace_path")?);
    if input.get("round").and_then(Value::as_str) == Some("escalation") {
        return escalation_prepare(runtime, &workspace, input)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()));
    }
    initial_prepare(runtime, &workspace, input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))
}

fn initial_prepare(
    runtime: &OrbitRuntime,
    workspace: &Path,
    input: &Value,
) -> Result<Value, OrbitError> {
    let prepared_records = input
        .get("prepared")
        .and_then(|prepared| prepared.get("records"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let dispatches = input
        .get("prepared")
        .and_then(|prepared| prepared.get("reviews"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let review_results = collect_result_objects(input.get("review_results"), "findings");
    let mut records = Vec::new();
    let mut partitions = Vec::new();
    for mut record in prepared_records {
        if record.get("outcome").and_then(Value::as_str) != Some("ready") {
            records.push(record);
            continue;
        }
        let task_id = string_field(&record, "task_id")?;
        let catalog = catalog_of(&record);
        let dispatch = dispatches
            .iter()
            .find(|dispatch| dispatch.get("task_id").and_then(Value::as_str) == Some(&task_id));
        let max_findings = record
            .pointer("/budget/max_findings")
            .and_then(Value::as_u64)
            .unwrap_or(15) as usize;
        let mut invalid_evidence_dropped = 0u32;
        let mut over_max_findings_dropped = 0u32;
        let mut findings: Vec<Value> = Vec::new();
        let reported = review_results
            .iter()
            .find(|result| result.get("task_id").and_then(Value::as_str) == Some(&task_id))
            .and_then(|result| result.get("findings"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for raw in &reported {
            match comparison_finding(raw, &catalog, workspace) {
                Ok(finding) => {
                    if findings.len() >= max_findings {
                        over_max_findings_dropped += 1;
                    } else {
                        findings.push(finding);
                    }
                }
                Err(_) => invalid_evidence_dropped += 1,
            }
        }
        if let Some(checks) = dispatch
            .and_then(|dispatch| dispatch.get("check_findings"))
            .and_then(Value::as_array)
        {
            for check in checks {
                findings.push(check_finding(check, &catalog));
            }
        }
        for (index, finding) in findings.iter_mut().enumerate() {
            finding["finding_id"] = json!(format!("f{}", index + 1));
            finding["verdict"] = unverified("not yet verified");
            finding["verified_severity"] = Value::Null;
            finding["severity_source"] = Value::Null;
        }
        let verifier = record
            .get("verifier_crew")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(crew) = &verifier {
            let scratch = record
                .get("scratch_dir")
                .and_then(Value::as_str)
                .map_or_else(|| workspace.join(".orbit/tmp"), PathBuf::from);
            for (index, batch) in findings.chunks(VISUAL_VERIFY_BATCH_SIZE).enumerate() {
                partitions.push(partition(
                    runtime, workspace, &scratch, &task_id, index, crew, batch, &record, false,
                )?);
            }
        } else {
            for finding in findings.iter_mut() {
                finding["verdict"] = unverified(
                    "no image-capable crew other than the reviewer's and the implementer's",
                );
            }
        }
        record["outcome"] = json!("reviewed");
        record["findings"] = json!(findings);
        record["invalid_evidence_dropped"] = json!(invalid_evidence_dropped);
        record["over_max_findings_dropped"] = json!(over_max_findings_dropped);
        records.push(record);
    }
    Ok(json!({
        "applies": !records.is_empty(),
        "partition_count": partitions.len(),
        "partitions": partitions,
        "records": records,
    }))
}

fn escalation_prepare(
    runtime: &OrbitRuntime,
    workspace: &Path,
    input: &Value,
) -> Result<Value, OrbitError> {
    let records = input
        .get("prepared")
        .and_then(|prepared| prepared.get("records"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = input.get("verify_results").cloned().unwrap_or(Value::Null);
    let mut partitions = Vec::new();
    for record in &records {
        let Some(crew) = record.get("escalation_crew").and_then(Value::as_str) else {
            continue;
        };
        let task_id = string_field(record, "task_id")?;
        let catalog = catalog_of(record);
        let verdicts = visual_verdicts_by_finding(&results, &catalog, workspace);
        let low: Vec<Value> = record
            .get("findings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|finding| {
                finding
                    .get("finding_id")
                    .and_then(Value::as_str)
                    .and_then(|id| verdicts.get(id))
                    .is_some_and(|verdict| verdict.confidence == "low")
            })
            .collect();
        let scratch = record
            .get("scratch_dir")
            .and_then(Value::as_str)
            .map_or_else(|| workspace.join(".orbit/tmp"), PathBuf::from);
        for (index, batch) in low.chunks(VISUAL_VERIFY_BATCH_SIZE).enumerate() {
            partitions.push(partition(
                runtime, workspace, &scratch, &task_id, index, crew, batch, record, true,
            )?);
        }
    }
    Ok(json!({
        "applies": !partitions.is_empty(),
        "partition_count": partitions.len(),
        "partitions": partitions,
        "records": records,
    }))
}

#[allow(clippy::too_many_arguments)]
fn partition(
    runtime: &OrbitRuntime,
    workspace: &Path,
    scratch: &Path,
    task_id: &str,
    index: usize,
    crew: &str,
    batch: &[Value],
    record: &Value,
    escalation: bool,
) -> Result<Value, OrbitError> {
    let dir = scratch.join(format!(
        "verify-{}-{index}",
        if escalation { "esc" } else { "init" }
    ));
    create_private_dir_all(&dir)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    let mut images: Vec<Value> = Vec::new();
    let mut staged: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for finding in batch {
        for evidence in finding
            .get("evidence")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(artifact) = evidence.get("image").and_then(Value::as_str) else {
                continue;
            };
            if seen.iter().any(|path| path == artifact) {
                continue;
            }
            seen.push(artifact.to_string());
            let Some(bytes) = runtime
                .get_task_artifact(task_id, artifact)?
                .map(|stored| stored.content)
            else {
                continue;
            };
            let leaf = Path::new(artifact)
                .file_name()
                .and_then(|leaf| leaf.to_str())
                .unwrap_or("image.png");
            let path = dir.join(format!("{:02}-{leaf}", images.len() + 1));
            atomic_write_bytes(&path, &bytes)
                .map_err(|error| OrbitError::Io(format!("stage {}: {error}", path.display())))?;
            images.push(json!(path.display().to_string()));
            staged.push(json!({"artifact": artifact}));
        }
    }
    let findings: Vec<Value> = batch
        .iter()
        .map(|finding| {
            let mut item = finding.clone();
            item["deviations"] = json!(touching_deviations(finding, record));
            item
        })
        .collect();
    Ok(json!({
        "task_id": task_id,
        "partition_index": index,
        "escalation": escalation,
        "workspace_path": workspace.display().to_string(),
        "crew": crew,
        "image_input": {"images": images},
        "staged_images": staged,
        "findings": findings,
        "catalog": record.get("catalog").cloned().unwrap_or_else(|| json!({})),
    }))
}

/// Declared deviations of the finding's mock whose region overlaps one
/// of its evidence regions (or that declare no region): the verifier
/// judges whether their basis holds (§2.5).
fn touching_deviations(finding: &Value, record: &Value) -> Vec<Value> {
    let mock = finding.get("mock").and_then(Value::as_str).unwrap_or("");
    let regions: Vec<[u32; 4]> = finding
        .get("evidence")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| region_of(item.get("region")?))
                .collect()
        })
        .unwrap_or_default();
    record
        .get("deviations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|deviation| {
            deviation.get("mock").and_then(Value::as_str) == Some(mock)
                && match deviation.get("region").and_then(region_of) {
                    None => true,
                    Some(region) => regions.iter().any(|other| regions_overlap(region, *other)),
                }
        })
        .collect()
}

pub(super) fn regions_overlap(left: [u32; 4], right: [u32; 4]) -> bool {
    left[0] < right[0].saturating_add(right[2])
        && right[0] < left[0].saturating_add(left[2])
        && left[1] < right[1].saturating_add(right[3])
        && right[1] < left[1].saturating_add(left[3])
}

pub(super) fn region_of(value: &Value) -> Option<[u32; 4]> {
    let items = value.as_array()?;
    let mut region = [0u32; 4];
    for (slot, item) in region.iter_mut().zip(items) {
        *slot = u32::try_from(item.as_u64()?).ok()?;
    }
    (items.len() == 4).then_some(region)
}

pub(super) fn string_field(value: &Value, field: &str) -> Result<String, OrbitError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OrbitError::InvalidInput(format!("visual record names no `{field}`")))
}

pub(super) fn catalog_of(record: &Value) -> VisualImageCatalog {
    record
        .get("catalog")
        .and_then(Value::as_object)
        .map(|catalog| {
            catalog
                .iter()
                .filter_map(|(path, size)| {
                    let size = size.as_array()?;
                    Some((
                        path.clone(),
                        (
                            u32::try_from(size.first()?.as_u64()?).ok()?,
                            u32::try_from(size.get(1)?.as_u64()?).ok()?,
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn unverified(reasoning: &str) -> Value {
    json!({
        "verdict": "unverified",
        "crew": Value::Null,
        "confidence": Value::Null,
        "severity": Value::Null,
        "escalated": false,
        "reasoning": reasoning,
        "deviation_rejected": false,
    })
}

/// One reviewer finding, validated (§11.1). An error drops it as
/// `invalid_evidence`.
fn comparison_finding(
    raw: &Value,
    catalog: &VisualImageCatalog,
    workspace: &Path,
) -> Result<Value, String> {
    let question = match raw.get("question") {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => text.trim().to_string(),
        _ => return Err("finding names no question".into()),
    };
    if !matches!(
        question.as_str(),
        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8"
    ) {
        return Err(format!("question {question:?} is not 1-8"));
    }
    let mock = non_empty(raw, "mock")?;
    let title = non_empty(raw, "title")?;
    let detail = non_empty(raw, "detail")?;
    let severity = non_empty(raw, "severity")?;
    if !SEVERITIES.contains(&severity.as_str()) {
        return Err(format!("severity {severity:?} is not on the shared scale"));
    }
    let evidence: Vec<VisualEvidence> = raw
        .get("evidence")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| format!("evidence does not parse: {error}"))?
        .unwrap_or_default();
    validate_visual_evidence(&evidence, catalog)?;
    for item in &evidence {
        if let Some(location) = &item.location {
            cited_line_exists(workspace, location)?;
        }
    }
    Ok(json!({
        "source": "comparison",
        "question": question,
        "mock": mock,
        "title": title,
        "detail": detail,
        "evidence": evidence,
        "severity": severity,
    }))
}

/// One deterministic check finding as a visual finding (§7, §11.2):
/// its evidence is its recorded region, else the whole capture.
fn check_finding(check: &Value, catalog: &VisualImageCatalog) -> Value {
    let mock = check
        .get("mock")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // The capture of the mock the check names (`<pass prefix>/<mock-name>/…`),
    // else any capture image in the catalog.
    let preferred = format!(
        "/{}/",
        crate::application::visual_capture::mock_output_name(&mock)
    );
    let chosen = catalog
        .iter()
        .find(|(image, _)| {
            image.starts_with("visual/")
                && image.contains(&preferred)
                && !image.contains("side-by-side")
        })
        .or_else(|| {
            catalog
                .iter()
                .find(|(image, _)| image.starts_with("visual/"))
        });
    let mut evidence = Vec::new();
    if let Some((image, (width, height))) = chosen {
        let region = check
            .get("region")
            .and_then(region_of)
            .filter(|region| orbit_types::task::visual_region_inside(*region, *width, *height))
            .unwrap_or([0, 0, *width, *height]);
        evidence.push(VisualEvidence {
            image: image.clone(),
            region,
            location: None,
        });
    }
    json!({
        "source": "check",
        "question": check.get("check").and_then(Value::as_str).unwrap_or("check:unknown"),
        "mock": mock,
        "title": check.get("title").and_then(Value::as_str).unwrap_or("check finding"),
        "detail": check.get("detail").and_then(Value::as_str).unwrap_or(""),
        "evidence": evidence,
        "severity": check.get("severity").and_then(Value::as_str).unwrap_or("medium"),
    })
}

fn non_empty(raw: &Value, field: &str) -> Result<String, String> {
    raw.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("finding names no {field}"))
}

/// Whether a cited `path:line` exists in the candidate checkout (§11.1).
pub(super) fn cited_line_exists(workspace: &Path, location: &str) -> Result<(), String> {
    let (path, line) = location
        .trim()
        .rsplit_once(':')
        .ok_or_else(|| format!("location {location:?} is not `path:line`"))?;
    let line: usize = line
        .parse()
        .ok()
        .filter(|line| *line > 0)
        .ok_or_else(|| format!("location {location:?} has no positive line"))?;
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err(format!("location {location:?} is not repository-relative"));
    }
    let content = std::fs::read_to_string(workspace.join(path))
        .map_err(|_| format!("location {location:?} names no file at the candidate"))?;
    if content.lines().count() < line {
        return Err(format!("location {location:?} is past the file's end"));
    }
    Ok(())
}

/// Flatten a fan-out collect value into the worker result objects that
/// carry `field` (an array, or one result, or nested under a collect
/// wrapper).
pub(super) fn collect_result_objects(value: Option<&Value>, field: &str) -> Vec<Value> {
    fn walk(value: &Value, field: &str, out: &mut Vec<Value>) {
        match value {
            Value::Array(items) => {
                for item in items {
                    walk(item, field, out);
                }
            }
            Value::Object(_) if value.get(field).is_some_and(Value::is_array) => {
                out.push(value.clone());
            }
            Value::Object(object) => {
                for nested in object.values() {
                    walk(nested, field, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    if let Some(value) = value {
        walk(value, field, &mut out);
    }
    out
}

/// A verifier verdict that passed validation (§11.2).
#[derive(Debug, Clone)]
pub(super) struct ParsedVisualVerdict {
    pub verdict: VisualVerdictKind,
    pub confidence: String,
    pub severity: Option<String>,
    pub reasoning: String,
    pub deviation_rejected: bool,
    pub rejection_reason: Option<String>,
}

/// Every valid verdict in a verifier fan-out result, by finding id.
/// Evidence must be an image region in this delivery's catalog or a
/// `path:line` at the candidate; CONFIRMED must rate a severity on
/// the shared scale. An invalid verdict is simply absent: its finding
/// stays `unverified`.
pub(super) fn visual_verdicts_by_finding(
    results: &Value,
    catalog: &VisualImageCatalog,
    workspace: &Path,
) -> BTreeMap<String, ParsedVisualVerdict> {
    let mut verdicts = BTreeMap::new();
    for result in collect_result_objects(Some(results), "verdicts") {
        let Some(items) = result.get("verdicts").and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            let Some(finding_id) = item.get("finding_id").and_then(Value::as_str) else {
                continue;
            };
            if let Ok(verdict) = parse_verdict(item, catalog, workspace) {
                verdicts.insert(finding_id.to_string(), verdict);
            }
        }
    }
    verdicts
}

fn parse_verdict(
    item: &Value,
    catalog: &VisualImageCatalog,
    workspace: &Path,
) -> Result<ParsedVisualVerdict, String> {
    let verdict = match item.get("verdict").and_then(Value::as_str) {
        Some("CONFIRMED") => VisualVerdictKind::Confirmed,
        Some("REJECTED") => VisualVerdictKind::Rejected,
        _ => return Err("verdict is not CONFIRMED or REJECTED".into()),
    };
    let confidence = non_empty(item, "confidence")?;
    if !matches!(confidence.as_str(), "high" | "medium" | "low") {
        return Err("confidence is not high, medium or low".into());
    }
    let reasoning = non_empty(item, "reasoning")?;
    let evidence = item
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in &evidence {
        let location = entry
            .get("location")
            .and_then(Value::as_str)
            .ok_or("evidence names no location")?;
        if parse_visual_region_location(location).is_some() {
            validate_visual_region_location(location, catalog)?;
        } else {
            cited_line_exists(workspace, location)?;
        }
    }
    let severity = item
        .get("severity")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut rejection_reason = None;
    match verdict {
        VisualVerdictKind::Confirmed => {
            if evidence.is_empty() {
                return Err("a CONFIRMED verdict must cite evidence".into());
            }
            match &severity {
                Some(severity) if SEVERITIES.contains(&severity.as_str()) => {}
                _ => return Err("a CONFIRMED verdict must rate severity".into()),
            }
        }
        VisualVerdictKind::Rejected => {
            let rejection = item
                .get("rejection")
                .ok_or("a REJECTED verdict must name its rejection")?;
            rejection_reason = Some(non_empty(rejection, "reason")?);
            non_empty(rejection, "detail")?;
        }
        VisualVerdictKind::Unverified => return Err("unverified is not a verdict".into()),
    }
    Ok(ParsedVisualVerdict {
        verdict,
        confidence,
        severity,
        reasoning,
        deviation_rejected: item
            .get("deviation_rejected")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        rejection_reason,
    })
}
