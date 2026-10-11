//! Pilot prerequisites resolved only within the assessed workspace.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus, is_valid_orb_task_id, validate_task_dependencies_with};
use serde_json::Value;

use crate::OrbitRuntime;

/// Collect whole task IDs from prerequisite prose, including multiple IDs
/// in one finding. A name alone never establishes workspace ownership.
pub(crate) fn pilot_blocker_task_ids(assessment: &Value) -> BTreeSet<String> {
    assessment["blocked_by"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|text| {
            text.split(|character: char| {
                !character.is_alphanumeric() && !matches!(character, '-' | '_')
            })
        })
        .filter(|id| id.starts_with("ORB-") && is_valid_orb_task_id(id))
        .map(str::to_string)
        .collect()
}

impl OrbitRuntime {
    /// Called under the assessed task's write lock and local targets' read locks.
    /// Existing dependencies remain intact; only unfinished local tasks add
    /// edges, and the resulting graph must pass the ordinary cycle check.
    pub(crate) fn pilot_blocker_dependencies(
        &self,
        task: &Task,
        named: &BTreeSet<String>,
    ) -> Result<Vec<String>, OrbitError> {
        let mut dependencies = task.dependencies();
        let mut additions = Vec::new();
        for id in named {
            let Some(target) = self.stores().tasks().get_task(id)? else {
                continue;
            };
            if matches!(
                target.status,
                TaskStatus::Proposed
                    | TaskStatus::Backlog
                    | TaskStatus::InProgress
                    | TaskStatus::Blocked
                    | TaskStatus::Review
            ) && !dependencies.contains(id)
            {
                dependencies.push(id.clone());
                additions.push(id.clone());
            }
        }
        if !additions.is_empty() {
            validate_task_dependencies_with(Some(&task.id), &dependencies, |id| {
                if !is_valid_orb_task_id(id) {
                    return Ok(None);
                }
                Ok::<_, OrbitError>(
                    self.stores()
                        .tasks()
                        .get_task(id)?
                        .map(|task| task.dependencies()),
                )
            })?;
        }
        Ok(additions)
    }

    /// Blocker prose in the latest applied pilot receipt, for operator
    /// approval output. A newer assessment replaces these warnings, including
    /// a clean assessment; ordinary comments do not supersede the receipt.
    pub fn task_pilot_blocker_warnings(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        for comment in self.get_task_comments(id)?.iter().rev() {
            if comment.by != "task-pilot" {
                continue;
            }
            let Some((header, body)) = comment.message.split_once('\n') else {
                continue;
            };
            let Some(operation) = header.strip_prefix("operation_id=") else {
                continue;
            };
            if operation.len() != 64 || !operation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(audit) = serde_json::from_str::<Value>(body) else {
                continue;
            };
            let Some(assessment) = audit.get("assessment").filter(|value| value.is_object()) else {
                continue;
            };
            return Ok(assessment["blocked_by"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_string)
                .collect());
        }
        Ok(Vec::new())
    }
}
