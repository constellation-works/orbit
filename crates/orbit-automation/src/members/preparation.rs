//! One material fingerprint used by scheduling, apply and readiness consumers.

use crate::{AutomationError, delivery::definition_epoch};
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

pub const CONTRACT: &str = "material_v1";

/// Only unstarted work carries material worth fingerprinting, and an explicit
/// no-diff tag opts a task out entirely.
pub fn eligible(task: &Task) -> bool {
    matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog)
        && !task
            .tags
            .iter()
            .any(|tag| matches!(tag.as_str(), "no-diff-expected" | "no-diff-needed"))
}

/// Dependency/decision evidence and repository instruction bytes are supplied
/// by Core. Comments, audit metadata and priority are deliberately absent.
pub fn fingerprint(
    task: &Task,
    source_revision: &str,
    dependencies: &Value,
    instructions: &str,
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

    definition_epoch(&json!({
        "contract": CONTRACT, "id": task.id, "title": task.title.trim(),
        "description": task.description.trim(), "criteria": task.acceptance_criteria,
        "plan": task.plan.trim(), "selectors": selectors, "tags": tags,
        "tools": required_tools, "type": task.task_type, "complexity": task.complexity,
        "crew": task.crew, "eligible": eligible(task), "relations": task.relations,
        "dependencies": dependencies, "instructions": instructions,
        "source_revision": source_revision,
    }))
}
