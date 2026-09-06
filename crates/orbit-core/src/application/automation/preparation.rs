//! Authoritative task/source inputs for the shared material fingerprint.

use super::source::Source;
use crate::OrbitRuntime;
use orbit_automation::{AutomationError, members::preparation};
use orbit_types::task::Task;
use serde_json::{Value, json};

pub(crate) fn fingerprint(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: &str,
) -> Result<String, AutomationError> {
    let source = Source::new(&runtime.paths().repo_root);

    let mut dependencies = Vec::new();

    for id in task.dependencies().iter().take(51) {
        if dependencies.len() == 50 {
            return Err(AutomationError::Deferred("dependency_scan_budget".into()));
        }
        let dependency = runtime.get_task(id)?;
        dependencies.push(json!({"id": id, "status": dependency.status,
            "relations": dependency.relations, "criteria": dependency.acceptance_criteria,
            "description": dependency.description, "plan": dependency.plan,
            "refs": dependency.external_refs, "pr_status": dependency.pr_status}));
    }

    dependencies.sort_by_key(|value| value["id"].as_str().unwrap_or_default().to_string());

    // The pinned tree includes every repository instruction, including nested
    // selectors. Dirty local instructions cannot certify this pinned source.
    let paths = source.git(&["ls-tree", "-r", "--name-only", revision])?;

    let mut instructions = Vec::new();

    for path in paths
        .lines()
        .filter(|path| matches!(path.rsplit('/').next(), Some("AGENTS.md" | "CLAUDE.md")))
    {
        if instructions.len() >= 50 {
            return Err(AutomationError::Deferred("instruction_scan_budget".into()));
        }
        instructions.push((
            path.to_string(),
            source.git(&["show", &format!("{revision}:{path}")])?,
        ));
    }

    // The crew a task would actually run under is part of its material input.
    let assignment = runtime.resolve_crew_for_task(None, task.crew.as_deref())?;
    dependencies.push(json!({"effective_assignment": {"crew": assignment.name,
        "model": assignment.assignment.model, "provider": assignment.assignment.provider}}));

    preparation::fingerprint(
        task,
        revision,
        &Value::Array(dependencies),
        &serde_json::to_string(&instructions)
            .map_err(|e| AutomationError::Evidence(e.to_string()))?,
    )
}
