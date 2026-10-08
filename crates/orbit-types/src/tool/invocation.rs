//! Attempt binding supplied by a managed runtime, independently of tool arguments.

use serde::{Deserialize, Serialize};

use crate::task::ExecutionLocation;

/// Transportable attempt identity. Receiving adapters must accept this only from
/// their runtime/session channel, never deserialize it from tool arguments or
/// infer it from editable job inputs. SSH establishes access; the owner still
/// validates this binding in the claim transaction on every mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerInvocation {
    pub owner_machine_id: String,
    pub owner_workspace_id: String,
    pub owner_destination: String,
    pub task_id: String,
    pub claim_id: String,
    pub execution: ExecutionLocation,
    /// The original leaf run, unchanged for detached children and step retries.
    pub bound_run_id: String,
}

impl WorkerInvocation {
    pub fn validate(&self) -> Result<(), String> {
        if [
            &self.owner_machine_id,
            &self.owner_workspace_id,
            &self.owner_destination,
            &self.task_id,
            &self.claim_id,
            &self.execution.machine_id,
            &self.bound_run_id,
        ]
        .iter()
        .any(|value| value.trim().is_empty() || value.trim() != value.as_str())
        {
            return Err("managed worker invocation has an incomplete binding".into());
        }
        Ok(())
    }

    /// Explicit arguments may confirm a binding, but cannot replace it.
    pub fn validate_arguments(&self, arguments: &serde_json::Value) -> Result<(), String> {
        self.validate()?;
        for (key, expected) in [
            ("task_id", &self.task_id),
            ("during_task", &self.task_id),
            ("claim_id", &self.claim_id),
            ("bound_run_id", &self.bound_run_id),
        ] {
            if let Some(value) = arguments.get(key)
                && value.as_str() != Some(expected.as_str())
            {
                return Err(format!(
                    "managed worker argument `{key}` conflicts with its binding"
                ));
            }
        }
        Ok(())
    }

    /// A claimed worker files only follow-up work spawned from its claimed
    /// task [ORB-14260]: a new task's `relations` must name the claimed task
    /// as `spawned_from`, so the worker cannot attach work to, or make work
    /// depend on, any other task. A worker whose claimed task files review
    /// findings (`findings`) may also name each culprit task as
    /// `regression_from` [ORB-14792]; only the holder of the claimed task can
    /// decide that, and must check the targets. Returns those targets.
    pub fn validate_spawned_relations(
        &self,
        arguments: &serde_json::Value,
        findings: bool,
    ) -> Result<Vec<String>, String> {
        const UNSPAWNED: &str = "a claimed worker's new task must be spawned_from its claimed task";
        const OTHER_RELATION: &str = "a claimed worker's new task may relate only to its \
            claimed task, as spawned_from; only a claimed review task's findings may also name \
            regression_from";
        const OTHER_FINDING_RELATION: &str = "a claimed review worker's new task may relate only \
            to its claimed task, as spawned_from, and to the task that introduced its finding, \
            as regression_from";
        let relations = arguments
            .get("relations")
            .and_then(serde_json::Value::as_array)
            .filter(|relations| !relations.is_empty())
            .ok_or(UNSPAWNED)?;
        let mut spawned = false;
        let mut culprits = Vec::new();
        for relation in relations {
            let target = relation.get("target").and_then(serde_json::Value::as_str);
            match relation.get("type").and_then(serde_json::Value::as_str) {
                Some("spawned_from") if target == Some(self.task_id.as_str()) => spawned = true,
                Some("regression_from") if findings && target.is_some() => {
                    culprits.extend(target.map(str::to_owned));
                }
                _ if findings => return Err(OTHER_FINDING_RELATION.into()),
                _ => return Err(OTHER_RELATION.into()),
            }
        }
        if !spawned {
            return Err(UNSPAWNED.into());
        }
        Ok(culprits)
    }
}

/// The coordination tools a claimed worker on another machine than its owner
/// reaches that owner with through its run's coordinator, the step runner's
/// broker [ORB-14260]. The agent sandbox masks the SSH credentials the owner
/// route needs, so these calls cross the broker instead, each scoped by the
/// broker's own records to the claimed task. The list is closed; any other
/// coordination tool from such a worker's sandbox is refused.
pub const CLAIMED_OWNER_TOOLS: [&str; 5] = [
    "orbit.task.show",
    "orbit.task.add",
    "orbit.friction.add",
    "orbit.task.artifact.get",
    "orbit.task.artifact.put",
];

/// Whether `tool` is one of the [`CLAIMED_OWNER_TOOLS`].
#[must_use]
pub fn is_claimed_owner_tool(tool: &str) -> bool {
    CLAIMED_OWNER_TOOLS.contains(&tool)
}

#[cfg(test)]
#[path = "tests/invocation.rs"]
mod tests;
