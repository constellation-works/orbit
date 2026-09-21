//! One material fingerprint used by scheduling, apply and readiness consumers.

use crate::{AutomationError, delivery::definition_epoch};
use orbit_types::task::Task;
use orbit_types::workflow::automation::members::PreparationEligibility;
use serde_json::{Value, json};

pub const CONTRACT: &str = "material_v1";

/// Only unstarted work carries material worth fingerprinting, and an explicit
/// no-diff tag opts a task out entirely — by default. The consumer's resolved
/// `eligibility` block narrows or widens that rule [ORB-12745]; every caller
/// that decides for the same consumer must evaluate the same resolved value.
pub fn eligible(task: &Task, eligibility: &PreparationEligibility) -> bool {
    eligibility.admits(task)
}

/// Dependency/decision evidence and repository instruction bytes are supplied
/// by Core. Comments, audit metadata and priority are deliberately absent.
///
/// The resolved eligibility is material input: a changed predicate must
/// invalidate assessments accepted under the old one. The default predicate
/// is the contract's baseline and adds nothing to the hash, so a definition
/// that gains an explicit-but-equivalent block, or a workspace upgrading from
/// the hard-coded rule, keeps every accepted assessment fresh.
pub fn fingerprint(
    task: &Task,
    source_revision: &str,
    dependencies: &Value,
    instructions: &str,
    eligibility: &PreparationEligibility,
) -> Result<String, AutomationError> {
    // Order-insensitive collections are normalized so an equivalent task fingerprints
    // identically regardless of how its lists were authored.
    let mut selectors = task.context_files.clone();
    selectors.sort();
    selectors.dedup();

    let mut tags = task.tags.clone();
    tags.sort();
    tags.dedup();

    let mut required_tools = task.required_tools.clone();
    required_tools.sort();
    required_tools.dedup();

    let mut material = json!({
        "contract": CONTRACT, "id": task.id, "title": task.title.trim(),
        "description": task.description.trim(), "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(), "selectors": selectors, "tags": tags,
        "tools": required_tools, "type": task.task_type, "complexity": task.complexity,
        "crew": task.crew, "eligible": eligible(task, eligibility), "relations": task.relations,
        "dependencies": dependencies, "instructions": instructions,
        "source_revision": source_revision,
    });
    let eligibility = eligibility.normalized();
    if !eligibility.is_default() {
        material["eligibility"] = serde_json::to_value(eligibility)
            .map_err(|error| AutomationError::Evidence(error.to_string()))?;
    }

    definition_epoch(&material)
}
