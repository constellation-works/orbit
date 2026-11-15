//! The forecast's dependency graph [ORB-18686].
//!
//! Each `blocked_by` edge resolves through the same registry-global status
//! projection and [`TaskReferenceIndex`] the drain's readiness predicate
//! (`task_dependencies_ready_with_index`) reads, once per forecast:
//!
//! - a target in the scenario's scheduled set is an in-forecast edge: the
//!   dependent waits for it to finish in each simulated drain;
//! - a `done` target is already satisfied;
//! - a reference the index cannot verify here keeps the drain's permissive
//!   treatment and is counted;
//! - every other target (in review, blocked, someday, unapproved proposed,
//!   rejected, archived, missing, or another workspace's open work) has no
//!   start time the forecast can simulate yet, so the dependent is
//!   unforecastable, as is everything downstream of it and every task on or
//!   behind a dependency cycle.
//!
//! A task the caller already cannot simulate (a blocked task without return
//! evidence) is unforecastable for its own reason, with the same
//! downstream effect.

use std::collections::{BTreeMap, HashMap};

use orbit_types::task::{Task, TaskReferenceIndex, TaskStatus};

/// The dependency graph of the forecast's tasks, per scenario.
pub(super) struct DependencyPlan {
    /// In-forecast dependencies of each task (indices), any scenario.
    pub(super) edges: Vec<Vec<usize>>,
    /// Why each task cannot be scheduled without proposed work.
    pub(super) backlog: Vec<Option<&'static str>>,
    /// Why each task cannot be scheduled once proposed work is approved.
    pub(super) approved: Vec<Option<&'static str>>,
    /// Edges each task keeps only because the index cannot verify them here.
    pub(super) unverifiable: Vec<u64>,
}

fn unscheduled_reason(status: Option<TaskStatus>) -> &'static str {
    match status {
        Some(TaskStatus::Review) => "review",
        Some(TaskStatus::Blocked) => "blocked",
        Some(TaskStatus::Someday) => "someday",
        Some(TaskStatus::Proposed) => "proposed",
        Some(TaskStatus::Rejected) => "rejected",
        Some(TaskStatus::Archived) => "archived",
        Some(TaskStatus::Backlog | TaskStatus::InProgress) => "external_work",
        Some(TaskStatus::Done) | None => "missing",
    }
}

/// Resolve `tasks`' dependencies. `in_progress` tasks are already running,
/// so their own edges never hold them back. `unschedulable[i]` names why
/// task `i` cannot be simulated whatever its dependencies.
pub(super) fn plan_forecast_dependencies(
    tasks: &[&Task],
    status_by_id: &BTreeMap<String, TaskStatus>,
    reference_index: &TaskReferenceIndex,
    unschedulable: &[Option<&'static str>],
) -> DependencyPlan {
    let index_of: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(index, task)| (task.id.as_str(), index))
        .collect();
    let len = tasks.len();
    let mut edges = vec![Vec::new(); len];
    let mut external: Vec<Option<&'static str>> = unschedulable.to_vec();
    let mut unverifiable = vec![0u64; len];
    for (index, task) in tasks.iter().enumerate() {
        if task.status == TaskStatus::InProgress {
            continue;
        }
        for dependency in task.dependencies() {
            if let Some(other) = index_of.get(dependency.as_str()) {
                edges[index].push(*other);
                continue;
            }
            let status = status_by_id.get(&dependency).copied();
            if status.is_some_and(TaskStatus::satisfies_dependency) {
                continue;
            }
            if reference_index.is_not_verifiable_here(task, &dependency, status_by_id) {
                unverifiable[index] += 1;
                continue;
            }
            external[index].get_or_insert_with(|| unscheduled_reason(status));
        }
        edges[index].sort_unstable();
        edges[index].dedup();
    }
    let scenario = |include_proposed: bool| {
        let scheduled =
            |index: usize| include_proposed || tasks[index].status != TaskStatus::Proposed;
        let mut reason = external.clone();
        for (index, deps) in edges.iter().enumerate() {
            if reason[index].is_none() && deps.iter().any(|dep| !scheduled(*dep)) {
                reason[index] = Some("proposed");
            }
        }
        // Kahn's order over the scheduled tasks: whatever it cannot reach
        // is on or behind a cycle.
        let mut indegree: Vec<usize> = edges.iter().map(Vec::len).collect();
        let mut dependents = vec![Vec::new(); len];
        for (index, deps) in edges.iter().enumerate() {
            for dep in deps {
                dependents[*dep].push(index);
            }
        }
        let mut queue: Vec<usize> = (0..len).filter(|index| indegree[*index] == 0).collect();
        let mut seen = vec![false; len];
        while let Some(index) = queue.pop() {
            seen[index] = true;
            let upstream = edges[index].iter().any(|dep| reason[*dep].is_some());
            if reason[index].is_none() && upstream {
                reason[index] = Some("upstream");
            }
            for dependent in &dependents[index] {
                indegree[*dependent] -= 1;
                if indegree[*dependent] == 0 {
                    queue.push(*dependent);
                }
            }
        }
        for index in 0..len {
            if !seen[index] && reason[index].is_none() {
                reason[index] = Some("cycle");
            }
        }
        reason
    };
    DependencyPlan {
        backlog: scenario(false),
        approved: scenario(true),
        edges,
        unverifiable,
    }
}
