//! Targeted re-check of a held assessment instead of a full re-pilot
//! [ORB-21124].
//!
//! Every applied receipt records a `recheck_basis`: the source revision it
//! was assessed at and the task fields it assessed (title, description,
//! criteria, context_files, tags, dependencies, relations). When an explicit
//! pilot (`orbit task repilot`, a triage `re_pilot` answer, or another
//! explicit request) targets a task whose latest assessment holds promotion,
//! prepare hands the pilot that assessment, its holding findings and a
//! field-level diff of the task since then. The pilot then only judges each
//! holding finding (`resolved`, `still_holds` or `changed`) and the fields the
//! diff invalidates; apply keeps every prior field the pilot did not return.
//!
//! Prepare falls back to a full pilot, and records why, when:
//! - the operator asked for one (`repilot --full`);
//! - there is no readable prior receipt, it holds nothing, or it predates
//!   the recorded basis;
//! - the prior assessment was itself a re-check (no chains);
//! - it is older than the run's `recheck_max_age_hours`;
//! - its source revision is unavailable, or any of its footprint paths
//!   changed between that revision and this run's;
//! - the title or description was rewritten below
//!   [`SCOPE_SIMILARITY_THRESHOLD`], or new `context_files` were added.
//!
//! A re-check assessment goes through exactly the validation, revision
//! proofs and atomic write of a full one; only the merge of kept fields is
//! added in front of them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_types::task::{DECISION_ANSWERED_EVENT, Task, TaskHistoryEntry};
use serde_json::{Map, Value, json};

use super::admission::OPERATOR_TOOL_CLASSIFICATIONS;
use super::promotion::{WARNING_FIELDS, finding_holds_promotion, warning_text};
use super::{CONTEXT_CREATION_RETAINED, CONTEXT_EVIDENCE_RETAINED, VALIDATION_TOOL_WARNINGS};
use crate::OrbitRuntime;

/// Prepared-snapshot fields: the mode prepare chose, why a full pilot was
/// chosen when a prior assessment exists, and that assessment's id.
pub(super) const PILOT_MODE: &str = "pilot_mode";
pub(super) const PILOT_MODE_REASON: &str = "pilot_mode_reason";
pub(super) const PRIOR_ASSESSMENT_ID: &str = "prior_assessment_id";
/// Prepared-snapshot field carrying the re-check input; the partition lists
/// the same inputs by task id under [`RECHECKS`].
pub(super) const RECHECK: &str = "recheck";
pub(super) const RECHECKS: &str = "rechecks";
/// Assessment fields: the mode the receipt was produced in and, in a
/// re-check, the pilot's verdict per prior holding finding and the prior
/// fields apply kept.
pub(crate) const MODE: &str = "mode";
/// Why a full assessment ran although a prior one existed.
pub(crate) const MODE_REASON: &str = "mode_reason";
/// The mode reason of a full assessment a pilot returned for a re-check
/// task: the re-check itself needed a full pilot.
pub(crate) const RECHECK_ESCALATED: &str = "recheck_escalated";
pub(super) const RECHECK_FINDINGS: &str = "recheck_findings";
pub(super) const RECHECK_KEPT_FIELDS: &str = "recheck_kept_fields";
/// Audit field: the task as the receipt assessed it.
pub(super) const RECHECK_BASIS: &str = "recheck_basis";

pub(crate) const MODE_FULL: &str = "full";
pub(crate) const MODE_RECHECK: &str = "recheck";

/// Default bound on a prior assessment's age for a re-check.
pub(super) const DEFAULT_RECHECK_MAX_AGE_HOURS: u64 = 72;
/// Below this word-set similarity a title or description counts as
/// rewritten, so the re-check cannot bound the scope change.
pub(super) const SCOPE_SIMILARITY_THRESHOLD: f64 = 0.6;

const VERDICTS: [&str; 3] = ["resolved", "still_holds", "changed"];

/// Assessment fields the host owns or recomputes on every apply; a re-check
/// never carries them forward from the prior receipt.
/// `operator_tool_classifications` is here too: it is keyed only by
/// criterion index and tool, so a kept `subject` entry would still match
/// an edited criterion that now requires invoking the tool. The pilot
/// must re-classify every current mention; a mention it leaves
/// unclassified fails safe to an operator hold.
const HOST_FIELDS: [&str; 15] = [
    "task_id",
    "context_files_before",
    VALIDATION_TOOL_WARNINGS,
    "selector_normalizations",
    "context_attachment_warnings",
    "pilot_normalizations",
    CONTEXT_CREATION_RETAINED,
    "context_reauthorization_required",
    CONTEXT_EVIDENCE_RETAINED,
    "explore_brief",
    OPERATOR_TOOL_CLASSIFICATIONS,
    MODE,
    MODE_REASON,
    PRIOR_ASSESSMENT_ID,
    RECHECK_KEPT_FIELDS,
];

/// What the run asked for: a forced full pilot, and the age bound.
pub(super) struct RecheckRequest {
    pub(super) full: bool,
    pub(super) max_age: Duration,
    pub(super) now: DateTime<Utc>,
}

/// Prepare's decision for one task.
pub(super) struct RecheckPlan {
    pub(super) mode: &'static str,
    /// Why a full pilot ran; `None` for a re-check.
    pub(super) reason: Option<&'static str>,
    pub(super) prior_assessment_id: Option<String>,
    /// The structured re-check input; `Some` only for a re-check.
    pub(super) input: Option<Value>,
}

impl RecheckPlan {
    fn full(reason: &'static str, prior: Option<&str>) -> Self {
        Self {
            mode: MODE_FULL,
            reason: Some(reason),
            prior_assessment_id: prior.map(str::to_string),
            input: None,
        }
    }

    /// Record the decision on a prepared task snapshot.
    pub(super) fn record(&self, snapshot: &mut Value) {
        snapshot[PILOT_MODE] = json!(self.mode);
        snapshot[PILOT_MODE_REASON] = json!(self.reason);
        snapshot[PRIOR_ASSESSMENT_ID] = json!(self.prior_assessment_id);
        if let Some(input) = &self.input {
            snapshot[RECHECK] = input.clone();
        }
    }
}

/// Decide whether `task` is re-checked against its latest assessment or
/// piloted in full at `source_revision`.
pub(super) fn plan(
    runtime: &OrbitRuntime,
    workspace_root: &Path,
    task: &Task,
    source_revision: Option<&str>,
    request: &RecheckRequest,
) -> Result<RecheckPlan, OrbitError> {
    let comments = runtime.get_task_pilot_comments(&task.id)?;
    let Some((comment, receipt)) = super::pilot_receipts(&comments).next() else {
        return Ok(RecheckPlan::full("no_prior_assessment", None));
    };
    let prior_id = Some(receipt.operation_id);
    if request.full {
        return Ok(RecheckPlan::full("operator_requested_full", prior_id));
    }
    let audit = serde_json::from_str::<Value>(receipt.body).unwrap_or_default();
    let Some(prior) = audit.get("assessment").filter(|value| value.is_object()) else {
        return Ok(RecheckPlan::full("prior_assessment_unreadable", prior_id));
    };
    let holding = holding_findings(prior);
    if holding.is_empty() {
        return Ok(RecheckPlan::full("prior_not_held", prior_id));
    }
    if prior[MODE] == MODE_RECHECK {
        return Ok(RecheckPlan::full("prior_was_recheck", prior_id));
    }
    let Some(basis) = audit.get(RECHECK_BASIS).filter(|value| value.is_object()) else {
        return Ok(RecheckPlan::full("prior_basis_unrecorded", prior_id));
    };
    if request.now - comment.at > request.max_age {
        return Ok(RecheckPlan::full("prior_too_old", prior_id));
    }
    let (Some(current), Some(prior_revision)) =
        (source_revision, basis["source_revision"].as_str())
    else {
        return Ok(RecheckPlan::full("source_revision_unavailable", prior_id));
    };
    match footprint_moved(
        workspace_root,
        prior_revision,
        current,
        &footprint_paths(prior),
    ) {
        None => return Ok(RecheckPlan::full("prior_revision_unavailable", prior_id)),
        Some(true) => return Ok(RecheckPlan::full("footprint_moved", prior_id)),
        Some(false) => {}
    }
    let history = runtime.get_task_history(&task.id)?;
    let diff = task_diff(basis, task, &history, comment.at);
    if let Some(reason) = unbounded_scope(&diff) {
        return Ok(RecheckPlan::full(reason, prior_id));
    }
    let prior_assessment = prior.clone();
    Ok(RecheckPlan {
        mode: MODE_RECHECK,
        reason: None,
        prior_assessment_id: prior_id.map(str::to_string),
        input: Some(json!({
            "prior_assessment_id": receipt.operation_id,
            "prior_assessed_at": comment.at,
            "prior_source_revision": prior_revision,
            "holding_findings": holding,
            "prior_assessment": prior_assessment,
            "task_diff": diff,
        })),
    })
}

/// Every finding of `assessment` that holds promotion, each with a stable
/// id (`<category>[<index>]`, or the category for a single-valued finding),
/// its category, text and evidence. The deterministic
/// `validation_tool_warnings` are recomputed on every prepare, so the pilot
/// never judges them.
pub(super) fn holding_findings(assessment: &Value) -> Vec<Value> {
    let mut findings = Vec::new();
    for field in WARNING_FIELDS {
        if field == VALIDATION_TOOL_WARNINGS {
            continue;
        }
        let Some(values) = assessment.get(field).and_then(Value::as_array) else {
            continue;
        };
        for (index, finding) in values.iter().enumerate() {
            if finding_holds_promotion(field, finding) {
                findings.push(json!({
                    "id": format!("{field}[{index}]"),
                    "category": field,
                    "text": warning_text(finding).map_or_else(|| finding.to_string(), str::to_string),
                    "evidence": finding.get("evidence").cloned().unwrap_or(Value::Null),
                    "finding": finding,
                }));
            }
        }
    }
    if let Some(duplicate) = assessment
        .get("duplicate_of")
        .filter(|value| value.is_object())
    {
        findings.push(json!({
            "id": "duplicate_of",
            "category": "duplicate_of",
            "text": format!(
                "duplicate of {}",
                duplicate["task_id"].as_str().unwrap_or("an unnamed task")
            ),
            "evidence": duplicate.get("evidence").cloned().unwrap_or(Value::Null),
            "finding": duplicate,
        }));
    }
    findings
}

/// Repository paths the assessment's selectors anchor.
fn footprint_paths(assessment: &Value) -> Vec<String> {
    assessment["context_files_after"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|selector| orbit_common::fs::selector::anchor_path(selector).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Whether any footprint path changed between the two revisions; `None`
/// when the prior revision is not a commit in this repository. A path's
/// blob or tree id is compared at both revisions, so an added, removed or
/// edited file, or any change under a directory, counts.
fn footprint_moved(
    workspace_root: &Path,
    prior: &str,
    current: &str,
    paths: &[String],
) -> Option<bool> {
    let spec = format!("{prior}^{{commit}}");
    let known = run_git(workspace_root, &["cat-file", "-e", &spec]).ok()?;
    if !known.success {
        return None;
    }
    if prior == current || paths.is_empty() {
        return Some(false);
    }
    let objects = |revision: &str| -> Option<String> {
        let mut args = vec!["ls-tree", revision, "--"];
        args.extend(paths.iter().map(String::as_str));
        let listed = run_git(workspace_root, &args).ok()?;
        listed.success.then_some(listed.stdout)
    };
    Some(objects(prior)? != objects(current)?)
}

/// The fields a receipt records as the task it assessed.
pub(super) fn recheck_basis(
    task: &Task,
    context_files: &[String],
    tags: &[String],
    source_revision: Option<&str>,
) -> Value {
    json!({
        "source_revision": source_revision,
        "title": task.title,
        "description": task.description.trim(),
        "acceptance_criteria": task.acceptance_criteria,
        "context_files": context_files,
        "tags": tags,
        "dependencies": dependency_ids(task),
        "relations": relation_keys(task),
    })
}

fn dependency_ids(task: &Task) -> Vec<String> {
    task.dependencies()
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn relation_keys(task: &Task) -> Vec<String> {
    task.relations
        .iter()
        .map(|relation| {
            let kind = orbit_common::protocol::serde_string_label(&relation.relation_type)
                .unwrap_or_default();
            format!("{kind}:{}", relation.target)
        })
        .collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn set_diff(before: &[String], after: &[String]) -> Value {
    let before_set: BTreeSet<&String> = before.iter().collect();
    let after_set: BTreeSet<&String> = after.iter().collect();
    json!({
        "added": after_set.difference(&before_set).collect::<Vec<_>>(),
        "removed": before_set.difference(&after_set).collect::<Vec<_>>(),
    })
}

/// Word-set Jaccard similarity of two texts; two empty texts are identical.
fn similarity(before: &str, after: &str) -> f64 {
    let words = |text: &str| -> BTreeSet<String> {
        text.split(|character: char| !character.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (before, after) = (words(before), words(after));
    let union = before.union(&after).count();
    if union == 0 {
        return 1.0;
    }
    before.intersection(&after).count() as f64 / union as f64
}

fn text_diff(before: &str, after: &str) -> Value {
    let changed = before.trim() != after.trim();
    let mut diff = json!({
        "changed": changed,
        "similarity": (similarity(before, after) * 1000.0).round() / 1000.0,
    });
    if changed {
        diff["before"] = json!(before.trim());
    }
    diff
}

/// The field-level change of `task` since `basis`, with the decisions
/// answered after the prior assessment at `since`.
fn task_diff(
    basis: &Value,
    task: &Task,
    history: &[TaskHistoryEntry],
    since: DateTime<Utc>,
) -> Value {
    let answered = history
        .iter()
        .filter(|entry| entry.event == DECISION_ANSWERED_EVENT && entry.at > since)
        .map(|entry| json!({"at": entry.at, "by": entry.by, "note": entry.note}))
        .collect::<Vec<_>>();
    json!({
        "title": text_diff(basis["title"].as_str().unwrap_or_default(), &task.title),
        "description": text_diff(
            basis["description"].as_str().unwrap_or_default(),
            &task.description,
        ),
        "acceptance_criteria": set_diff(&strings(&basis["acceptance_criteria"]), &task.acceptance_criteria),
        "context_files": set_diff(&strings(&basis["context_files"]), &task.context_files),
        "tags": set_diff(&strings(&basis["tags"]), &task.tags),
        "dependencies": set_diff(&strings(&basis["dependencies"]), &dependency_ids(task)),
        "relations": set_diff(&strings(&basis["relations"]), &relation_keys(task)),
        "answered_decisions": answered,
    })
}

/// Why the diff touches scope beyond what a re-check can bound, if it does.
fn unbounded_scope(diff: &Value) -> Option<&'static str> {
    let rewritten = |field: &str| {
        diff[field]["similarity"]
            .as_f64()
            .is_some_and(|similarity| similarity < SCOPE_SIMILARITY_THRESHOLD)
    };
    if rewritten("title") || rewritten("description") {
        return Some("scope_rewritten");
    }
    if diff["context_files"]["added"]
        .as_array()
        .is_some_and(|added| !added.is_empty())
    {
        return Some("scope_added");
    }
    None
}

/// Complete a re-check assessment from its input. A result without
/// `recheck_findings` is the pilot's full assessment and is recorded as one
/// ([`RECHECK_ESCALATED`]). Otherwise require a verdict for every prior
/// holding finding, then keep each prior field the pilot did not
/// return. A kept finding field drops the findings judged `resolved` and
/// replaces those judged `changed` with their `current` finding. The mode
/// and prior assessment id are written by the host, never taken from the
/// pilot.
pub(super) fn complete_recheck(assessment: &mut Value, recheck: &Value) -> Result<(), String> {
    let prior_id = recheck["prior_assessment_id"]
        .as_str()
        .ok_or("prepared re-check input has no prior_assessment_id")?;
    // A pilot that found the re-check unbounded returns a complete full
    // assessment instead: it is validated and recorded as a full one.
    if assessment.get(RECHECK_FINDINGS).is_none() {
        mark_full(assessment, Some(RECHECK_ESCALATED), Some(prior_id));
        return Ok(());
    }
    let holding = recheck["holding_findings"]
        .as_array()
        .ok_or("prepared re-check input has no holding_findings")?;
    let Value::Object(fields) = assessment else {
        return Err("re-check assessment must be an object".into());
    };
    let verdicts = fields
        .get(RECHECK_FINDINGS)
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!("re-check assessment must return {RECHECK_FINDINGS}, one verdict per prior holding finding")
        })?;
    let known: BTreeSet<&str> = holding
        .iter()
        .filter_map(|finding| finding["id"].as_str())
        .collect();
    let mut by_id: BTreeMap<String, Value> = BTreeMap::new();
    for verdict in verdicts {
        let id = verdict["id"]
            .as_str()
            .ok_or_else(|| format!("each {RECHECK_FINDINGS} entry needs a string id"))?;
        if !known.contains(id) {
            return Err(format!(
                "{RECHECK_FINDINGS} names {id}, which is not a prior holding finding"
            ));
        }
        let status = verdict["status"].as_str().unwrap_or_default();
        if !VERDICTS.contains(&status) {
            return Err(format!(
                "{RECHECK_FINDINGS} {id} status must be resolved, still_holds or changed"
            ));
        }
        if verdict["evidence"]
            .as_str()
            .is_none_or(|evidence| evidence.trim().is_empty())
        {
            return Err(format!("{RECHECK_FINDINGS} {id} needs non-empty evidence"));
        }
        if status == "changed" && verdict.get("current").is_none_or(Value::is_null) {
            return Err(format!(
                "{RECHECK_FINDINGS} {id} is changed but carries no current finding"
            ));
        }
        if by_id.insert(id.to_string(), verdict.clone()).is_some() {
            return Err(format!("{RECHECK_FINDINGS} judges {id} twice"));
        }
    }
    if let Some(missing) = known.iter().find(|id| !by_id.contains_key(**id)) {
        return Err(format!(
            "{RECHECK_FINDINGS} has no verdict for prior holding finding {missing}"
        ));
    }
    let prior = recheck["prior_assessment"]
        .as_object()
        .ok_or("prepared re-check input has no prior_assessment")?;
    let mut kept = Vec::new();
    for (key, value) in prior {
        if HOST_FIELDS.contains(&key.as_str()) || fields.contains_key(key) {
            continue;
        }
        let value = kept_value(key, value, &by_id);
        fields.insert(key.clone(), value);
        kept.push(key.clone());
    }
    fields.insert(MODE.into(), json!(MODE_RECHECK));
    fields.insert(PRIOR_ASSESSMENT_ID.into(), json!(prior_id));
    fields.insert(RECHECK_KEPT_FIELDS.into(), json!(kept));
    Ok(())
}

fn kept_value(key: &str, value: &Value, verdicts: &BTreeMap<String, Value>) -> Value {
    let verdict = |id: &str| verdicts.get(id).map(|verdict| verdict["status"].as_str());
    if key == "duplicate_of" {
        return match verdict(key) {
            Some(Some("resolved")) => Value::Null,
            Some(Some("changed")) => verdicts[key]["current"].clone(),
            _ => value.clone(),
        };
    }
    let Some(values) = value.as_array().filter(|_| WARNING_FIELDS.contains(&key)) else {
        return value.clone();
    };
    let mut kept = Vec::with_capacity(values.len());
    for (index, finding) in values.iter().enumerate() {
        let id = format!("{key}[{index}]");
        match verdict(&id) {
            Some(Some("resolved")) => {}
            Some(Some("changed")) => kept.push(verdicts[&id]["current"].clone()),
            _ => kept.push(finding.clone()),
        }
    }
    Value::Array(kept)
}

/// Record the mode of a full assessment: the host's, never the pilot's.
pub(super) fn mark_full(assessment: &mut Value, reason: Option<&str>, prior: Option<&str>) {
    if let Value::Object(fields) = assessment {
        fields.insert(MODE.into(), json!(MODE_FULL));
        fields.remove(RECHECK_FINDINGS);
        fields.remove(RECHECK_KEPT_FIELDS);
        match prior {
            Some(prior) => fields.insert(PRIOR_ASSESSMENT_ID.into(), json!(prior)),
            None => fields.remove(PRIOR_ASSESSMENT_ID),
        };
        // A reason is recorded only against a prior assessment it explains.
        match reason.filter(|_| prior.is_some()) {
            Some(reason) => fields.insert(MODE_REASON.into(), json!(reason)),
            None => fields.remove(MODE_REASON),
        };
    }
}

/// The partition field listing each re-check task's input.
pub(super) fn partition_rechecks<'a>(
    ids: impl Iterator<Item = &'a String>,
    snapshots: &[Value],
) -> Option<Map<String, Value>> {
    let rechecks = ids
        .filter_map(|id| {
            snapshots
                .iter()
                .find(|snapshot| snapshot["task_id"].as_str() == Some(id.as_str()))
                .and_then(|snapshot| snapshot.get(RECHECK))
                .map(|input| (id.clone(), input.clone()))
        })
        .collect::<Map<_, _>>();
    (!rechecks.is_empty()).then_some(rechecks)
}
