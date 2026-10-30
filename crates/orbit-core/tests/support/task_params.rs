//! A minimal [`TaskCreateParams`] for orbit-core's unit tests (`lib.rs`) and the
//! `runtime` integration binary, which cannot import `cfg(test)` library helpers.

use orbit_store::contracts::TaskCreateParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use serde_json::json;

/// A medium-priority chore authored by `test`, with no relations, tags,
/// context files or optional metadata. The record store assigns the id.
///
/// Only the fields without a serde default are named here, so a new optional
/// field with a default needs no edit here or at a call site. Scenarios set
/// the few fields they care about with struct update syntax:
/// `TaskCreateParams { complexity, ..task_create_params(title, description, status) }`.
pub(crate) fn task_create_params(
    title: impl Into<String>,
    description: impl Into<String>,
    status: TaskStatus,
) -> TaskCreateParams {
    serde_json::from_value(json!({
        "actor": "test",
        "title": title.into(),
        "description": description.into(),
        "acceptance_criteria": [],
        "dependencies": [],
        "relations": [],
        "tags": [],
        "required_tools": [],
        "plan": "",
        "execution_summary": "",
        "context_files": [],
        "created_by": "test",
        "status": status,
        "priority": TaskPriority::Medium,
        "task_type": TaskType::Chore,
        "external_refs": [],
        "comments": [],
    }))
    .expect("task create fixture matches the contract")
}
