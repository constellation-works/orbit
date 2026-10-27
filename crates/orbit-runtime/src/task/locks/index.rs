//! Lock surfaces, overlap lookup and conflict indexing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_common::fs::overlap_index::OverlapIndex;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{TaskLockConflict, TaskLockHolder};
use orbit_types::task::{
    EpicHierarchyNode, NO_DIFF_EXPECTED_TAG, Task, TaskEnvelopeV2, TaskRelationType, TaskStatus,
    inherited_only_epic_roots,
};
use serde::Serialize;

use super::unlocked::UnlockedContextPaths;
use crate::RuntimeKernel;
use crate::task::{DeclaredContextFiles, declared_context_files};

/// An `in-progress` or `review` task holds its context against other work,
/// unless it carries [`NO_DIFF_EXPECTED_TAG`] [ORB-14247].
pub(super) fn holds_context_lock(status: TaskStatus, tags: &[String]) -> bool {
    matches!(status, TaskStatus::InProgress | TaskStatus::Review) && !context_lock_exempt(tags)
}

pub(super) fn context_lock_exempt(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
}

/// Return the effective lock surface for one task.
///
/// Every task — leaf, child, or `epic`-tagged root — reserves exactly what it
/// declares. Hierarchy is metadata: a parent never inherits a child's
/// footprint, so conflict admission excludes only the work that genuinely
/// overlaps [ORB-12491]. Declarations `unlocked` matches are left out unless
/// the task declares nothing else (`UnlockedContextPaths::lock_surface`).
pub fn lock_context_files_for_task(
    task: &Task,
    workspace_root: &Path,
    unlocked: &UnlockedContextPaths,
) -> Vec<String> {
    canonical_lock_surface(
        declared_context_files(&task.context_files, workspace_root).retained,
        unlocked,
    )
}

/// Apply admission's unlocked rules to canonical declarations, retaining the
/// docs-only fallback and a stable, deduplicated surface for every consumer.
pub(super) fn canonical_lock_surface(
    declarations: impl IntoIterator<Item = String>,
    unlocked: &UnlockedContextPaths,
) -> Vec<String> {
    unlocked.lock_surface(
        declarations
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    )
}

/// One requested selector of a candidate that overlaps a selector an
/// `in-progress` / `review` task holds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TaskLockOverlap {
    pub requested_file: String,
    pub locking_task_id: String,
}

/// Selector -> the `in-progress` / `review` tasks holding it.
///
/// Expand each active surface exactly once. Expansion is the expensive half —
/// every selector is checked against the filesystem — so automatic admission
/// and `orbit task eligible` both build this map once and read it rather than
/// expanding again. Holder lists are sorted and deduplicated.
///
/// A task tagged [`NO_DIFF_EXPECTED_TAG`] is not a holder [ORB-14247]. It
/// still waits on its own dependencies, on locks other tasks hold, and on
/// its claim.
pub fn active_task_lock_holders<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    workspace_root: &Path,
    unlocked: &UnlockedContextPaths,
) -> BTreeMap<String, Vec<String>> {
    let mut holders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks {
        if holds_context_lock(task.status, &task.tags) {
            for file in lock_context_files_for_task(task, workspace_root, unlocked) {
                holders.entry(file).or_default().push(task.id.clone());
            }
        }
    }
    for locking_task_ids in holders.values_mut() {
        locking_task_ids.sort();
        locking_task_ids.dedup();
    }
    holders
}

/// The holders keyed by anchor, so one candidate's overlap check is a prefix
/// lookup per requested selector rather than a pass over every held one.
pub fn lock_holder_index(lock_holders: &BTreeMap<String, Vec<String>>) -> OverlapIndex<&[String]> {
    let mut index = OverlapIndex::new();
    for (selector, locking_task_ids) in lock_holders {
        index.insert(selector, locking_task_ids.as_slice());
    }
    index
}

/// Every (requested selector, holder) pair where `task`'s lock surface
/// overlaps a held selector at a point `lock_modes` does not share, sorted
/// and deduplicated. Empty means the task collides with no active work.
pub fn task_lock_overlaps(
    task: &Task,
    holders: &OverlapIndex<&[String]>,
    workspace_root: &Path,
    unlocked: &UnlockedContextPaths,
    lock_modes: &FootprintLockModes,
) -> Vec<TaskLockOverlap> {
    let mut conflicts = Vec::new();
    for requested_file in lock_context_files_for_task(task, workspace_root, unlocked) {
        for (_, locking_task_ids) in holders.overlapping(&requested_file, lock_modes) {
            for locking_task_id in locking_task_ids.iter() {
                conflicts.push(TaskLockOverlap {
                    requested_file: requested_file.clone(),
                    locking_task_id: locking_task_id.clone(),
                });
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

/// Envelope metadata indexed for lock-surface expansion: active tasks,
/// explicitly requested tasks, and their ancestors. One operation builds it
/// once without hydrating task bodies or sidecars; repeated surface expansion
/// then reuses it.
pub struct TaskLockIndex {
    tasks: BTreeMap<String, TaskEnvelopeV2>,
    /// The inherited-only `epic` roots in the whole workspace, decided while
    /// every envelope was still in hand. The retained `tasks` deliberately keep
    /// only active, requested, and ancestor envelopes, which is not enough to
    /// see a root's *descendants* — so the rule is answered once at load rather
    /// than re-read per reservation.
    inherited_only_epic_root_ids: BTreeSet<String>,
    /// `workflow.unlocked_context_paths` as the runtime admitted it.
    unlocked: UnlockedContextPaths,
    /// `workflow.lock_modes` as the runtime admitted it.
    lock_modes: FootprintLockModes,
}

impl TaskLockIndex {
    pub fn load(
        runtime: &RuntimeKernel,
        requested_task_ids: &[String],
    ) -> Result<Self, OrbitError> {
        let envelopes = runtime
            .task_candidates(&Default::default(), usize::MAX)?
            .items;
        Ok(Self::from_envelopes(
            envelopes,
            requested_task_ids,
            runtime.unlocked_context_paths().clone(),
            runtime.footprint_lock_modes().clone(),
        ))
    }

    fn from_envelopes(
        envelopes: Vec<TaskEnvelopeV2>,
        requested_task_ids: &[String],
        unlocked: UnlockedContextPaths,
        lock_modes: FootprintLockModes,
    ) -> Self {
        let all_tasks = envelopes
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>();
        let requested_task_ids = requested_task_ids.iter().collect::<BTreeSet<_>>();
        let seed_ids = all_tasks
            .values()
            .filter(|task| {
                matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
                    || requested_task_ids.contains(&task.id)
            })
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        let mut retained_ids = seed_ids.clone();

        // Parent envelopes are kept so hierarchy stays readable from the index
        // under the same guarded walk the bundle-backed implementation used.
        // They do not widen anyone's lock surface [ORB-12491].
        for task_id in retained_ids.clone() {
            retain_task_ancestors(&task_id, &all_tasks, &mut retained_ids);
        }

        let inherited_only_epic_root_ids =
            inherited_only_epic_roots(all_tasks.values().map(|task| EpicHierarchyNode {
                id: task.id.as_str(),
                parent_id: envelope_parent_id(task),
                tags: &task.tags,
                declares_context: !task.context_files.is_empty(),
            }))
            .into_keys()
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();

        let tasks = all_tasks
            .into_iter()
            .filter(|(task_id, _)| retained_ids.contains(task_id))
            .collect::<BTreeMap<_, _>>();
        Self {
            tasks,
            inherited_only_epic_root_ids,
            unlocked,
            lock_modes,
        }
    }

    /// The index without `task_ids` as holders, for tasks that hold their
    /// context by status alone and have taken no file of it. A task named
    /// here that a caller still requests stays readable.
    pub fn without_status_holders(
        mut self,
        task_ids: &BTreeSet<String>,
        requested_task_ids: &[String],
    ) -> Self {
        self.tasks.retain(|task_id, _| {
            !task_ids.contains(task_id) || requested_task_ids.contains(task_id)
        });
        self
    }

    pub(crate) fn get(&self, task_id: &str) -> Option<&TaskEnvelopeV2> {
        self.tasks.get(task_id)
    }

    pub(crate) fn tasks(&self) -> impl Iterator<Item = &TaskEnvelopeV2> {
        self.tasks.values()
    }

    pub(super) fn into_active_lock_surfaces(
        mut self,
        workspace_root: &Path,
    ) -> Vec<(TaskEnvelopeV2, Vec<String>)> {
        let mut active_ids = self
            .tasks
            .values()
            .filter(|task| holds_context_lock(task.status, &task.tags))
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        active_ids.sort_by_key(|task_id| {
            self.tasks.get(task_id).map(|task| {
                (
                    task_lock_status_rank(task.status),
                    task.created_at,
                    task.id.clone(),
                )
            })
        });

        active_ids
            .into_iter()
            .filter_map(|task_id| {
                let files = self
                    .tasks
                    .get(&task_id)
                    .map(|task| self.lock_context_files(task, workspace_root))?;
                self.tasks.remove(&task_id).map(|task| (task, files))
            })
            .collect()
    }

    /// [`lock_context_files_for_task`] over indexed envelopes.
    pub(crate) fn lock_context_files(
        &self,
        task: &TaskEnvelopeV2,
        workspace_root: &Path,
    ) -> Vec<String> {
        self.declared_lock_surface(task, workspace_root).retained
    }

    /// The canonical lock surface for `task`, less what
    /// `workflow.unlocked_context_paths` leaves unlocked, plus the
    /// declarations that could not be canonicalized at all.
    ///
    /// Invalid entries are the only ones a lock surface loses, and they are
    /// reported rather than dropped in silence: a task whose every declaration
    /// is unusable would otherwise read as a claim protecting no files.
    fn declared_lock_surface(
        &self,
        task: &TaskEnvelopeV2,
        workspace_root: &Path,
    ) -> DeclaredContextFiles {
        let mut declared = declared_context_files(&task.context_files, workspace_root);
        declared.retained = canonical_lock_surface(declared.retained, &self.unlocked);
        declared.invalid = declared
            .invalid
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        declared
    }

    /// Whether `task_id` has declared any `context_files` entries at all.
    ///
    /// A selector for a file the task has not created yet is a declaration
    /// like any other and reaches [`Self::lock_context_files`] intact, so this
    /// answers the narrower question a task-scope reservation refuses on:
    /// nothing declared at all. A root inherits nothing from its children, so
    /// an empty root declares no surface [ORB-12491].
    pub(crate) fn declares_context_surface(&self, task_id: &str) -> bool {
        self.tasks
            .get(task_id)
            .is_some_and(|task| !task.context_files.is_empty())
    }

    /// Whether `task_id` is one of the workspace's inherited-only `epic` roots:
    /// tagged, declaring nothing of its own, with descendants that do declare
    /// context ([`inherited_only_epic_roots`]).
    pub(crate) fn is_inherited_only_epic_root(&self, task_id: &str) -> bool {
        self.inherited_only_epic_root_ids.contains(task_id)
    }
}

fn envelope_parent_id(task: &TaskEnvelopeV2) -> Option<&str> {
    task.relations
        .iter()
        .find(|relation| relation.relation_type == TaskRelationType::ChildOf)
        .map(|relation| relation.target.as_str())
}

fn retain_task_ancestors(
    task_id: &str,
    task_lookup: &BTreeMap<String, TaskEnvelopeV2>,
    retained_ids: &mut BTreeSet<String>,
) {
    let mut visited = BTreeSet::from([task_id.to_string()]);
    let mut next_parent_id = task_lookup.get(task_id).and_then(envelope_parent_id);
    for _ in 0..32 {
        let Some(parent_id) = next_parent_id else {
            break;
        };
        if !visited.insert(parent_id.to_string()) {
            break;
        }
        let Some(parent) = task_lookup.get(parent_id) else {
            break;
        };
        retained_ids.insert(parent.id.clone());
        next_parent_id = envelope_parent_id(parent);
    }
}

pub fn requested_task_files_indexed(
    index: &TaskLockIndex,
    task_ids: &[String],
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    let mut requested_files = BTreeSet::new();
    for task_id in task_ids {
        let task = index
            .get(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.clone()))?;
        requested_files.extend(index.lock_context_files(task, workspace_root));
    }
    Ok(requested_files.into_iter().collect())
}

pub fn task_lock_conflicts_indexed(
    index: &TaskLockIndex,
    bundle_task_ids: &[String],
    requested_files: &[String],
    workspace_root: &Path,
) -> Vec<TaskLockConflict> {
    let mut conflicts = Vec::new();
    visit_task_lock_conflicts(
        index,
        bundle_task_ids,
        requested_files,
        workspace_root,
        &mut |file, task_id| {
            conflicts.push(TaskLockConflict {
                file: file.to_string(),
                held_by: TaskLockHolder::Task,
                held_by_id: task_id.to_string(),
            });
        },
    );
    conflicts.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.held_by_id.cmp(&right.held_by_id))
    });
    conflicts
}

/// [`task_lock_conflicts_indexed`] for an answer that can show at most
/// `shown` conflicts: the first `shown` in (selector, task id) order plus
/// the first task on every other blocked selector, so each blocked selector
/// stays blocked without holding every selector-by-task pair. `None` is
/// every conflict.
pub(crate) fn task_lock_conflicts_bounded(
    index: &TaskLockIndex,
    requested_files: &[String],
    workspace_root: &Path,
    shown: Option<usize>,
) -> Vec<TaskLockConflict> {
    let Some(shown) = shown else {
        return task_lock_conflicts_indexed(index, &[], requested_files, workspace_root);
    };
    let mut first = BTreeSet::<(String, String)>::new();
    let mut first_per_file = BTreeMap::<String, String>::new();
    visit_task_lock_conflicts(
        index,
        &[],
        requested_files,
        workspace_root,
        &mut |file, task_id| {
            first_per_file
                .entry(file.to_string())
                .and_modify(|held_by| {
                    if task_id < held_by.as_str() {
                        *held_by = task_id.to_string();
                    }
                })
                .or_insert_with(|| task_id.to_string());
            first.insert((file.to_string(), task_id.to_string()));
            if first.len() > shown {
                first.pop_last();
            }
        },
    );
    first
        .into_iter()
        .chain(first_per_file)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|(file, held_by_id)| TaskLockConflict {
            file,
            held_by: TaskLockHolder::Task,
            held_by_id,
        })
        .collect()
}

/// Hand `visit` each (requested selector, task id) pair where a task outside
/// `bundle_task_ids` holds a context lock blocking that selector.
fn visit_task_lock_conflicts(
    index: &TaskLockIndex,
    bundle_task_ids: &[String],
    requested_files: &[String],
    workspace_root: &Path,
    visit: &mut dyn FnMut(&str, &str),
) {
    let bundle_ids = bundle_task_ids.iter().cloned().collect::<BTreeSet<_>>();
    let requested_files = requested_files.iter().cloned().collect::<BTreeSet<_>>();
    if requested_files.is_empty() {
        return;
    }
    for task in index.tasks().filter(|task| {
        holds_context_lock(task.status, &task.tags) && !bundle_ids.contains(&task.id)
    }) {
        let held_files = index.lock_context_files(task, workspace_root);
        for requested_file in &requested_files {
            if held_files
                .iter()
                .any(|held_file| index.lock_modes.selectors_block(requested_file, held_file))
            {
                visit(requested_file, &task.id);
            }
        }
    }
}

pub fn merge_task_lock_conflicts(
    left: Vec<TaskLockConflict>,
    right: Vec<TaskLockConflict>,
) -> Vec<TaskLockConflict> {
    let mut merged = left;
    merged.extend(right);
    merged.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| holder_rank(a.held_by).cmp(&holder_rank(b.held_by)))
            .then(a.held_by_id.cmp(&b.held_by_id))
    });
    merged.dedup_by(|a, b| {
        a.file == b.file && a.held_by == b.held_by && a.held_by_id == b.held_by_id
    });
    merged
}

/// Task locks first, then reservations, then hard leases.
fn holder_rank(holder: TaskLockHolder) -> u8 {
    match holder {
        TaskLockHolder::Task => 0,
        TaskLockHolder::Reservation => 1,
        TaskLockHolder::Lease => 2,
    }
}

/// A task-scope reservation whose bundle declares no `context_files` at all
/// would otherwise mint a real reservation ID that holds nothing — a silent
/// "0 file(s)" success that looks like a claim was taken when it was not.
/// Refuse it by name instead so the caller declares context or falls back to
/// explicit `--file` selectors.
/// Every declared selector on the requested bundle, as stored, that cannot be
/// canonicalized against the workspace root.
pub(super) fn invalid_declared_selectors(
    index: &TaskLockIndex,
    task_ids: &[String],
    workspace_root: &Path,
) -> Vec<String> {
    let mut invalid = BTreeSet::new();
    for task_id in task_ids {
        if let Some(task) = index.get(task_id) {
            invalid.extend(index.declared_lock_surface(task, workspace_root).invalid);
        }
    }
    invalid.into_iter().collect()
}

pub(super) fn task_lock_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Review => 1,
        _ => 2,
    }
}
