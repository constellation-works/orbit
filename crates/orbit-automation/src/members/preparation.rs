//! One material fingerprint used by scheduling, apply and readiness consumers.

use std::collections::BTreeMap;

use crate::{AutomationError, delivery::json_definition_epoch};
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::automation::members::{
    MaterialField, PreparationEligibility, PreparationPolicy, SourceSensitivity,
};
use serde_json::{Value, json};

pub const CONTRACT: &str = "material_v2";

/// The contract accepted assessments were certified under before the material
/// set became configurable [ORB-13638]. Kept only so such an assessment can be
/// carried forward instead of re-piloting every assessed task on upgrade.
const LEGACY_CONTRACT: &str = "material_v1";

/// Only unstarted work carries material worth fingerprinting, and an explicit
/// no-diff tag opts a task out entirely — by default. The consumer's resolved
/// `eligibility` block narrows or widens that rule [ORB-12745]; every caller
/// that decides for the same consumer must evaluate the same resolved value.
pub fn eligible(task: &Task, eligibility: &PreparationEligibility) -> bool {
    eligibility.admits(task)
}

/// Evidence Core reads from outside the task. Each value is consulted only
/// when the policy makes it material, so Core may leave the rest `Null`.
#[derive(Debug, Clone, Default)]
pub struct MaterialEvidence {
    /// The branch-head identity: the commit for `any`, the object id of each
    /// selector path for `context_files`, `Null` for `ignore`.
    pub source: Value,
    /// Resolved dependency statuses and meaning (`dependencies`).
    pub dependencies: Value,
    /// The model/provider the task's crew resolves to (`crew`).
    pub assignment: Value,
    /// Pinned repository instruction bytes (`instructions`).
    pub instructions: Value,
}

/// Hash the task inputs `policy.freshness` names [ORB-13638]. Comments, audit
/// metadata and priority are never material. Eligibility is not freshness:
/// it decides whether a task is fingerprinted at all, and only its verdict is
/// hashed, so retagging a still-eligible task changes nothing by default.
///
/// A non-default eligibility or freshness is itself material input: changing
/// either invalidates assessments accepted under the old one. The defaults add
/// nothing to the hash.
pub fn fingerprint(
    task: &Task,
    evidence: &MaterialEvidence,
    policy: &PreparationPolicy,
) -> Result<String, AutomationError> {
    let freshness = policy.freshness.normalized();
    let mut material = json!({
        "contract": CONTRACT, "id": task.id,
        "eligible": eligible(task, &policy.eligibility),
    });
    for field in &freshness.material_fields {
        let (key, value) = field_value(field, task, evidence);
        material[key] = value;
    }
    match freshness.source_sensitivity {
        SourceSensitivity::Ignore => {}
        SourceSensitivity::Any => material["source_revision"] = evidence.source.clone(),
        SourceSensitivity::ContextFiles => {
            material["source_selectors"] = evidence.source.clone();
        }
    }
    if !freshness.is_default() {
        material["freshness"] = serde_json::to_value(&freshness)
            .map_err(|error| AutomationError::Evidence(error.to_string()))?;
    }
    let eligibility = policy.eligibility.normalized();
    if !eligibility.is_default() {
        material["eligibility"] = serde_json::to_value(eligibility)
            .map_err(|error| AutomationError::Evidence(error.to_string()))?;
    }

    json_definition_epoch(material)
}

/// Companion to the material fingerprint for a task-pilot's bounded status
/// retry. Every material field remains in the hash except the task status's
/// contribution to eligibility and resolved dependency statuses.
pub fn fingerprint_ignoring_status(
    task: &Task,
    evidence: &MaterialEvidence,
    policy: &PreparationPolicy,
) -> Result<String, AutomationError> {
    let mut task = task.clone();
    task.status = TaskStatus::Proposed;
    let mut evidence = evidence.clone();
    evidence.dependencies = without_dependency_status(evidence.dependencies);
    fingerprint(&task, &evidence, policy)
}

/// Per-field digests of the inputs `policy.freshness` names.
///
/// Each digest is the field value `fingerprint` hashes, so a named component
/// is one of those inputs and not a second definition of it. Eligibility is
/// not a component. Dependency status is not either: the status-neutral retry
/// treats a dependency status change as non-material, and naming it here
/// would report that retry as a dependency edit.
pub fn component_digests(
    task: &Task,
    evidence: &MaterialEvidence,
    policy: &PreparationPolicy,
) -> Result<BTreeMap<String, String>, AutomationError> {
    let freshness = policy.freshness.normalized();
    let mut components = BTreeMap::new();
    for field in &freshness.material_fields {
        let (key, value) = field_value(field, task, evidence);
        let value = if *field == MaterialField::Dependencies {
            without_dependency_status(value)
        } else {
            value
        };
        components.insert(key.to_string(), json_definition_epoch(value)?);
    }
    if freshness.source_sensitivity != SourceSensitivity::Ignore {
        components.insert(
            "source".to_string(),
            json_definition_epoch(evidence.source.clone())?,
        );
    }
    Ok(components)
}

fn field_value(
    field: &MaterialField,
    task: &Task,
    evidence: &MaterialEvidence,
) -> (&'static str, Value) {
    match field {
        MaterialField::Title => ("title", json!(task.title.trim())),
        MaterialField::Description => ("description", json!(task.description.trim())),
        MaterialField::Criteria => ("criteria", json!(task.acceptance_criteria)),
        MaterialField::Plan => ("plan", json!(task.plan.trim())),
        MaterialField::Selectors => ("selectors", json!(sorted(&task.context_files))),
        MaterialField::Tags => ("tags", json!(sorted(&task.tags))),
        MaterialField::Crew => (
            "crew",
            json!({"crew": task.crew, "assignment": evidence.assignment}),
        ),
        MaterialField::Tools => ("tools", json!(sorted(&task.required_tools))),
        MaterialField::Type => ("type", json!(task.task_type)),
        MaterialField::Complexity => ("complexity", json!(task.complexity)),
        MaterialField::Relations => ("relations", json!(task.relations)),
        MaterialField::Dependencies => ("dependencies", evidence.dependencies.clone()),
        MaterialField::Instructions => ("instructions", evidence.instructions.clone()),
    }
}

fn without_dependency_status(mut value: Value) -> Value {
    if let Some(entries) = value.as_array_mut() {
        for entry in entries {
            if let Some(status) = entry.get_mut("status") {
                *status = Value::Null;
            }
        }
    }
    value
}

/// The exact `material_v1` hash: every task field, the eligibility verdict,
/// dependency evidence with the crew assignment appended, instructions and
/// the source revision. Only an accepted pre-upgrade assessment is checked
/// against it, at the revision that assessment pinned.
pub fn legacy_fingerprint(
    task: &Task,
    source_revision: &str,
    dependencies: &Value,
    instructions: &str,
    eligibility: &PreparationEligibility,
) -> Result<String, AutomationError> {
    let mut material = json!({
        "contract": LEGACY_CONTRACT, "id": task.id, "title": task.title.trim(),
        "description": task.description.trim(), "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(), "selectors": sorted(&task.context_files),
        "tags": sorted(&task.tags), "tools": sorted(&task.required_tools),
        "type": task.task_type, "complexity": task.complexity,
        "crew": task.crew, "eligible": eligible(task, eligibility), "relations": task.relations,
        "dependencies": dependencies, "instructions": instructions,
        "source_revision": source_revision,
    });
    let eligibility = eligibility.normalized();
    if !eligibility.is_default() {
        material["eligibility"] = serde_json::to_value(eligibility)
            .map_err(|error| AutomationError::Evidence(error.to_string()))?;
    }

    json_definition_epoch(material)
}

/// Order-insensitive collections are normalized so an equivalent task
/// fingerprints identically regardless of how its lists were authored.
fn sorted(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values.dedup();
    values
}
