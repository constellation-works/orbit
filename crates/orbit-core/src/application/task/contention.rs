//! Lock contention across a pending task population.
//!
//! Throughput under conflict-aware dispatch is bounded by how much of the
//! backlog can hold non-overlapping locks at once. Whether the backlog is
//! draining is a separate question from why it drains at the rate it does;
//! this answers the second by naming the selectors pending tasks keep
//! colliding on, and by measuring how far the population decomposes into
//! clusters that can never block one another.
//!
//! Surfaces come from the same expansion conflict admission uses — declared
//! selectors, pruned of paths that no longer exist. A contention picture
//! admission does not share is not worth reading.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_types::task::{Task, TaskStatus};

use crate::OrbitRuntime;
use crate::runtime::task::locks::lock_context_files_for_task;

/// One pending task reduced to the fields the analysis reads.
pub(crate) struct LockSurface {
    pub task_id: String,
    pub selectors: Vec<String>,
}

/// A selector that more than one pending task contends for.
pub struct LockContentionHotspot {
    /// The contended selector, in canonical form.
    pub selector: String,
    /// IDs of the pending tasks whose surface overlaps it, ascending.
    pub task_ids: Vec<String>,
}

impl LockContentionHotspot {
    /// How many pending tasks contend for this selector.
    pub fn tasks(&self) -> usize {
        self.task_ids.len()
    }
}

/// What caps parallelism across a pending population.
pub struct LockContentionReport {
    /// Contended selectors, most contended first, then by selector.
    pub hotspots: Vec<LockContentionHotspot>,
    /// Pending tasks declaring at least one surviving selector.
    pub constrained: usize,
    /// Pending tasks declaring nothing, which contend with no one.
    pub unconstrained: usize,
    /// Disjoint clusters among the constrained tasks.
    pub groups: usize,
    /// Task count in the largest such cluster.
    pub largest_group: usize,
}

impl LockContentionReport {
    /// A floor on how many pending tasks can hold locks simultaneously: one
    /// per disjoint cluster, plus every task that locks nothing at all.
    ///
    /// It is a floor and not the true maximum because two tasks inside one
    /// cluster are often compatible with each other — they are linked through
    /// a third. Computing the exact maximum is the maximum independent set
    /// problem; the floor is cheap, sound, and enough to act on.
    pub fn parallel_floor(&self) -> usize {
        self.groups + self.unconstrained
    }

    /// Total pending tasks the report covers.
    pub fn pending(&self) -> usize {
        self.constrained + self.unconstrained
    }
}

impl OrbitRuntime {
    /// Lock contention across every task whose status appears in `statuses`.
    ///
    /// The whole task table is loaded regardless of `statuses`, because
    /// parent-chain reads span tasks at any status. Only the selected tasks
    /// are expanded.
    pub fn task_lock_contention(
        &self,
        statuses: &[TaskStatus],
    ) -> Result<LockContentionReport, OrbitError> {
        let lookup: BTreeMap<String, Task> = self
            .list_tasks()?
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect();
        let repo_root = self.paths().repo_root.as_path();
        let surfaces: Vec<LockSurface> = lookup
            .values()
            .filter(|task| statuses.contains(&task.status))
            .map(|task| LockSurface {
                task_id: task.id.clone(),
                selectors: lock_context_files_for_task(task, repo_root),
            })
            .collect();
        Ok(compute_contention(&surfaces))
    }
}

/// Whether two lock surfaces would refuse to be held at the same time.
fn surfaces_conflict(left: &LockSurface, right: &LockSurface) -> bool {
    left.selectors
        .iter()
        .any(|a| right.selectors.iter().any(|b| overlaps(a, b)))
}

/// Rank selectors by how many surfaces overlap them, and cluster the surfaces
/// that transitively conflict.
///
/// Both passes are quadratic in the pending population. That is deliberate:
/// selector overlap is a containment relation — `dir:src` covers everything
/// beneath it — not string equality, so it cannot be bucketed by hash. The
/// input is one workspace's pending tasks, which is the scale this is for.
pub(crate) fn compute_contention(surfaces: &[LockSurface]) -> LockContentionReport {
    let constrained: Vec<&LockSurface> = surfaces
        .iter()
        .filter(|surface| !surface.selectors.is_empty())
        .collect();
    let unconstrained = surfaces.len() - constrained.len();

    let distinct: BTreeSet<&String> = constrained
        .iter()
        .flat_map(|surface| surface.selectors.iter())
        .collect();
    let mut hotspots: Vec<LockContentionHotspot> = distinct
        .into_iter()
        .filter_map(|selector| {
            let task_ids: Vec<String> = constrained
                .iter()
                .filter(|surface| {
                    surface
                        .selectors
                        .iter()
                        .any(|held| overlaps(held, selector))
                })
                .map(|surface| surface.task_id.clone())
                .collect();
            // A selector only one task claims constrains nothing.
            (task_ids.len() > 1).then(|| LockContentionHotspot {
                selector: selector.clone(),
                task_ids,
            })
        })
        .collect();
    // Most contended first; equal rows stay in selector order so repeated runs
    // over an unchanged backlog render identically.
    hotspots.sort_by(|a, b| {
        b.tasks()
            .cmp(&a.tasks())
            .then_with(|| a.selector.cmp(&b.selector))
    });

    let (groups, largest_group) = cluster(&constrained);

    LockContentionReport {
        hotspots,
        constrained: constrained.len(),
        unconstrained,
        groups,
        largest_group,
    }
}

/// Connected components over the conflict graph, as
/// `(component count, largest component size)`.
fn cluster(surfaces: &[&LockSurface]) -> (usize, usize) {
    let mut seen = vec![false; surfaces.len()];
    let mut groups = 0;
    let mut largest = 0;
    for start in 0..surfaces.len() {
        if seen[start] {
            continue;
        }
        groups += 1;
        let mut size = 0;
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(index) = stack.pop() {
            size += 1;
            for (other, surface) in surfaces.iter().enumerate() {
                if !seen[other] && surfaces_conflict(surfaces[index], surface) {
                    seen[other] = true;
                    stack.push(other);
                }
            }
        }
        largest = largest.max(size);
    }
    (groups, largest)
}
