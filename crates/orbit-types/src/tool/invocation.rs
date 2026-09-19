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
}

#[cfg(test)]
#[path = "tests/invocation.rs"]
mod tests;
