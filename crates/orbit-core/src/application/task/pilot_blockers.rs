//! Pilot prerequisites resolved only within the assessed workspace.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus, is_valid_orb_task_id, validate_task_dependencies_with};
use serde_json::Value;

use crate::OrbitRuntime;

fn task_id_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '-' | '_')
}

/// Drop self-only prerequisites and remove whole self-ID tokens from mixed
/// findings, keeping the other prerequisite IDs and their evidence. Malformed
/// findings remain for the pilot contract validator to reject.
pub(crate) fn normalize_pilot_self_blockers(assessment: &mut Value, task_id: &str) -> bool {
    let Some(findings) = assessment
        .get_mut("blocked_by")
        .and_then(Value::as_array_mut)
    else {
        return false;
    };
    let mut dropped = false;
    findings.retain_mut(|finding| {
        let Some(text) = finding.as_str() else {
            return true;
        };
        let ids = text.split(|character| !task_id_character(character));
        if !ids.clone().any(|id| id == task_id) {
            return true;
        }
        dropped = true;
        if !ids
            .clone()
            .any(|id| id != task_id && is_valid_orb_task_id(id))
        {
            return false;
        }
        let mut retained = String::new();
        for part in text.split_inclusive(|character| !task_id_character(character)) {
            let token = part.trim_end_matches(|character| !task_id_character(character));
            if token == task_id {
                retained.push_str(&part[token.len()..]);
            } else {
                retained.push_str(part);
            }
        }
        *finding = Value::String(retained);
        true
    });
    dropped
}

/// Collect whole task IDs from prerequisite prose, including multiple IDs
/// in one finding. A name alone never establishes workspace ownership.
pub(crate) fn pilot_blocker_task_ids(assessment: &Value) -> BTreeSet<String> {
    assessment["blocked_by"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .flat_map(|text| text.split(|character| !task_id_character(character)))
        .filter(|id| id.starts_with("ORB-") && is_valid_orb_task_id(id))
        .map(str::to_string)
        .collect()
}

/// The part of the dependency graph the pilot's cycle check reads: every task
/// reachable from the assessed task's dependencies and named local blockers.
///
/// Pilot persistence locks these tasks in one sorted order before it validates,
/// so two applications whose graphs overlap queue on their first common task
/// instead of each holding a task the other's walk needs.
pub(crate) struct PilotDependencyGraph {
    tasks: BTreeMap<String, Task>,
    /// Reached IDs that named no task in this workspace when walked.
    absent: BTreeSet<String>,
}

impl PilotDependencyGraph {
    /// The existing tasks to lock.
    pub(crate) fn task_ids(&self) -> impl Iterator<Item = &String> {
        self.tasks.keys()
    }

    /// Existing dependencies remain intact; only unfinished local tasks add
    /// edges, and the resulting graph must pass the ordinary cycle check.
    /// Reads nothing beyond this snapshot.
    pub(crate) fn blocker_dependencies(
        &self,
        task: &Task,
        named: &BTreeSet<String>,
    ) -> Result<Vec<String>, OrbitError> {
        let mut dependencies = task.dependencies();
        let mut additions = Vec::new();
        for id in named {
            let Some(target) = self.tasks.get(id) else {
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
                Ok::<_, OrbitError>(self.tasks.get(id).map(Task::dependencies))
            })?;
        }
        Ok(additions)
    }
}

impl OrbitRuntime {
    /// Discover the graph by reading every reached task. Hold no task lock.
    pub(crate) fn discover_pilot_dependency_graph(
        &self,
        task: &Task,
        named: &BTreeSet<String>,
    ) -> Result<PilotDependencyGraph, OrbitError> {
        let graph = self.walk_pilot_dependency_graph(task, named, None)?;
        graph.ok_or_else(|| OrbitError::Execution("pilot graph discovery stopped".to_string()))
    }

    /// Read the graph again while holding the locks of `locked`, reading only
    /// its tasks. Reaching an ID outside it returns `None`: reading that task
    /// now would take its lock out of order, so the caller releases its locks
    /// and discovers again.
    pub(crate) fn reread_pilot_dependency_graph(
        &self,
        task: &Task,
        named: &BTreeSet<String>,
        locked: &PilotDependencyGraph,
    ) -> Result<Option<PilotDependencyGraph>, OrbitError> {
        self.walk_pilot_dependency_graph(task, named, Some(locked))
    }

    fn walk_pilot_dependency_graph(
        &self,
        task: &Task,
        named: &BTreeSet<String>,
        locked: Option<&PilotDependencyGraph>,
    ) -> Result<Option<PilotDependencyGraph>, OrbitError> {
        let mut graph = PilotDependencyGraph {
            tasks: BTreeMap::new(),
            absent: BTreeSet::new(),
        };
        let mut pending = task.dependencies();
        pending.extend(named.iter().cloned());
        while let Some(id) = pending.pop() {
            if !is_valid_orb_task_id(&id)
                || graph.tasks.contains_key(&id)
                || graph.absent.contains(&id)
            {
                continue;
            }
            if let Some(locked) = locked {
                if locked.absent.contains(&id) {
                    graph.absent.insert(id);
                    continue;
                }
                if !locked.tasks.contains_key(&id) {
                    return Ok(None);
                }
            }
            match self.stores().tasks().get_task(&id)? {
                Some(found) => {
                    pending.extend(found.dependencies());
                    graph.tasks.insert(id, found);
                }
                None => {
                    graph.absent.insert(id);
                }
            }
        }
        Ok(Some(graph))
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
