//! Request-scoped task selection and fully validated response rows (ORB-11205).

use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskComment, TaskEnvelopeV2, TaskHistoryEntry,
    TaskOrigin, TaskPriority, TaskRelationType, TaskStatus, TaskType, normalize_task_tags,
};

use super::TaskIndexFilter;

/// Predicates answered by envelope metadata. `None` statuses means all statuses.
#[derive(Debug, Clone, Default)]
pub struct TaskListFilter {
    /// Continue the canonical created-descending, ID-ascending scan.
    pub scan_before: Option<(chrono::DateTime<chrono::Utc>, String)>,
    /// Case-insensitive ID/title substring matched from envelope metadata.
    pub search: Option<String>,
    pub statuses: Option<Vec<TaskStatus>>,
    pub priority: Option<TaskPriority>,
    pub task_type: Option<TaskType>,
    /// Keep only tasks recorded with this origin; tasks without one never match.
    pub origin: Option<TaskOrigin>,
    /// Keep only the task carrying this rule-result fingerprint; tasks
    /// without one never match.
    pub rule_fingerprint: Option<String>,
    pub parent_id: Option<String>,
    pub job_run_id: Option<String>,
    pub tags: Vec<String>,
    /// Keep only tasks carrying at least one of these tags (case-insensitive,
    /// like `tags`). Empty means no such restriction.
    pub any_tags: Vec<String>,
    pub external_ref: Option<ExternalRef>,
    pub has_external_ref_system: Option<String>,
    /// Order tasks in a terminal status (done, archived, rejected) after the
    /// rest, each partition newest first. This is the status-aware default
    /// listing; `scan_before` continuation assumes the canonical order and is
    /// not combined with it.
    pub terminal_last: bool,
    /// Inclusive lower schedule bound; unscheduled tasks do not match.
    pub not_before_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Inclusive upper schedule bound; unscheduled tasks do not match.
    pub not_before_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Keep schedules strictly after this instant and order soonest first.
    /// Takes precedence over `terminal_last`; not combined with `scan_before`.
    pub scheduled_after: Option<chrono::DateTime<chrono::Utc>>,
}

impl TaskListFilter {
    /// Sort an already-loaded snapshot in this filter's listing order.
    ///
    /// The canonical order is newest first, with task ID ascending for ties.
    /// `scheduled_after` orders schedules soonest first; otherwise
    /// `terminal_last` moves terminal tasks behind non-terminal tasks. Each
    /// partition retains the canonical order. No filtering or I/O is done.
    pub fn sort_snapshot(&self, snapshot: &mut [TaskEnvelopeV2]) {
        self.sort_task_listing(
            snapshot,
            |task| task.not_before,
            |task| &task.created_at,
            |task| &task.id,
            |task| task.status,
        );
    }

    /// Filter, count and bound an already-loaded, ordered snapshot.
    ///
    /// `snapshot` must be ordered as by [`Self::sort_snapshot`] with this
    /// filter. All metadata predicates are applied, with counts as defined by
    /// [`TaskCandidates`]. Only the selected page's envelopes are cloned; the
    /// rest are borrowed to count matches. No I/O or bundle validation is done.
    pub fn select_ordered_snapshot(
        &self,
        snapshot: &[TaskEnvelopeV2],
        limit: usize,
    ) -> TaskCandidates {
        select_task_snapshot_items(snapshot, self, limit, |task| task, TaskEnvelopeV2::clone)
    }

    pub(crate) fn select_owned_snapshot(
        &self,
        snapshot: Vec<TaskEnvelopeV2>,
        limit: usize,
    ) -> TaskCandidates {
        // Move all matches before sorting; applying the cursor first cannot
        // change their relative order or either count.
        let mut candidates =
            select_task_snapshot_items(snapshot, self, usize::MAX, |task| task, |task| task);
        self.sort_snapshot(&mut candidates.items);
        candidates.items.truncate(limit);
        candidates
    }

    pub(crate) fn sort_task_listing<T>(
        &self,
        items: &mut [T],
        not_before: impl Fn(&T) -> Option<chrono::DateTime<chrono::Utc>>,
        created_at: impl Fn(&T) -> &chrono::DateTime<chrono::Utc>,
        id: impl Fn(&T) -> &str,
        status: impl Fn(&T) -> TaskStatus,
    ) {
        sort_task_created_desc_id_asc(items, created_at, id);
        if self.scheduled_after.is_some() {
            items.sort_by_key(not_before);
        } else if self.terminal_last {
            items.sort_by_key(|item| status(item).is_terminal());
        }
    }

    pub(crate) fn normalized(&self) -> Self {
        Self {
            tags: normalize_task_tags(self.tags.clone()),
            any_tags: normalize_task_tags(self.any_tags.clone()),
            search: self
                .search
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_lowercase),
            ..self.clone()
        }
    }

    pub(crate) fn matches(&self, task: &TaskEnvelopeV2) -> bool {
        self.matches_cursor(task)
            && self
                .not_before_from
                .is_none_or(|from| task.not_before.is_some_and(|at| at >= from))
            && self
                .not_before_until
                .is_none_or(|until| task.not_before.is_some_and(|at| at <= until))
            && self
                .scheduled_after
                .is_none_or(|now| task.not_before.is_some_and(|at| at > now))
            && self.search.as_ref().is_none_or(|query| {
                task.id.to_lowercase().contains(query) || task.title.to_lowercase().contains(query)
            })
            && self
                .statuses
                .as_ref()
                .is_none_or(|values| values.contains(&task.status))
            && self.priority.is_none_or(|value| task.priority == value)
            && self.task_type.is_none_or(|value| task.task_type == value)
            && self.origin.is_none_or(|value| task.origin == Some(value))
            && self
                .rule_fingerprint
                .as_ref()
                .is_none_or(|value| task.rule_fingerprint.as_ref() == Some(value))
            && self.parent_id.as_ref().is_none_or(|value| {
                task.relations.iter().any(|relation| {
                    relation.relation_type == TaskRelationType::ChildOf && relation.target == *value
                })
            })
            && self
                .job_run_id
                .as_ref()
                .is_none_or(|value| task.job_run_id.as_ref() == Some(value))
            && self.tags.iter().all(|tag| {
                task.tags
                    .iter()
                    .any(|available| available.trim().to_lowercase() == *tag)
            })
            && (self.any_tags.is_empty()
                || task
                    .tags
                    .iter()
                    .any(|available| self.any_tags.contains(&available.trim().to_lowercase())))
            && self.external_ref.as_ref().is_none_or(|value| {
                task.external_refs
                    .iter()
                    .any(|candidate| candidate.system == value.system && candidate.id == value.id)
            })
            && self.has_external_ref_system.as_ref().is_none_or(|value| {
                task.external_refs
                    .iter()
                    .any(|candidate| candidate.system == *value)
            })
    }

    /// The predicates the generated index answers in SQL. Whatever `matches`
    /// checks beyond these is applied to the selected envelopes afterwards.
    pub(crate) fn index_filter(&self, excluded_ids: Vec<String>) -> TaskIndexFilter {
        TaskIndexFilter {
            statuses: self.statuses.clone().unwrap_or_default(),
            priority: self.priority,
            job_run_id: self.job_run_id.clone(),
            tags: self.tags.clone(),
            any_tags: self.any_tags.clone(),
            parent_id: self.parent_id.clone(),
            scan_before: self.scan_before.clone(),
            excluded_ids,
        }
    }

    /// Whether [`index_filter`](Self::index_filter) covers every predicate, so
    /// the index can also bound the selection with `LIMIT` and count the total.
    /// An explicit empty status set matches nothing and is left to `matches`.
    /// The index narrows a `parent_id` selection to the tasks its relation
    /// rows name after the freshness scan verifies every `child_of` target.
    /// Parent selections still confirm each envelope before counting/limiting.
    pub(crate) fn is_fully_indexed(&self) -> bool {
        self.not_before_from.is_none()
            && self.not_before_until.is_none()
            && self.scheduled_after.is_none()
            && self.search.is_none()
            && self.task_type.is_none()
            && self.origin.is_none()
            && self.rule_fingerprint.is_none()
            && self.parent_id.is_none()
            && self.external_ref.is_none()
            && self.has_external_ref_system.is_none()
            && self
                .statuses
                .as_ref()
                .is_none_or(|values| !values.is_empty())
    }

    pub(crate) fn without_cursor(&self) -> Self {
        Self {
            scan_before: None,
            ..self.clone()
        }
    }

    pub(crate) fn matches_cursor(&self, task: &TaskEnvelopeV2) -> bool {
        self.scan_before.as_ref().is_none_or(|(at, id)| {
            task.created_at < *at || (task.created_at == *at && task.id > *id)
        })
    }
}

/// Shared canonical ordering for snapshot selection and generated-index reads.
pub(crate) fn sort_task_created_desc_id_asc<T>(
    items: &mut [T],
    created_at: impl Fn(&T) -> &chrono::DateTime<chrono::Utc>,
    id: impl Fn(&T) -> &str,
) {
    items.sort_by(|a, b| {
        created_at(b)
            .cmp(created_at(a))
            .then_with(|| id(a).cmp(id(b)))
    });
}

fn select_task_snapshot_items<T>(
    snapshot: impl IntoIterator<Item = T>,
    filter: &TaskListFilter,
    limit: usize,
    envelope: impl Fn(&T) -> &TaskEnvelopeV2,
    into_envelope: impl Fn(T) -> TaskEnvelopeV2,
) -> TaskCandidates {
    let filter = filter.normalized();
    let unbounded = filter.without_cursor();
    let mut candidates = TaskCandidates::default();
    for item in snapshot {
        let task = envelope(&item);
        if !unbounded.matches(task) {
            continue;
        }
        candidates.total_without_cursor += 1;
        if !filter.matches_cursor(task) {
            continue;
        }
        candidates.total += 1;
        if candidates.items.len() < limit {
            candidates.items.push(into_envelope(item));
        }
    }
    candidates
}

/// How `query_task_rows` materializes the rows that fill a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskListHydration {
    /// Read each selected task's bundle (body documents and sidecars).
    #[default]
    Full,
    /// Build each row from its candidate envelope only: no bundle read,
    /// with empty body documents and sidecars. Every field of a compact
    /// task-list row is envelope-derived, so a page projected to those
    /// fields is identical to a hydrated page.
    EnvelopeOnly,
}

/// Ordered metadata matches. Counts do not certify off-page bundle integrity.
#[derive(Debug, Clone, Default)]
pub struct TaskCandidates {
    pub items: Vec<TaskEnvelopeV2>,
    /// Matches remaining after `scan_before`, before `limit`.
    pub total: usize,
    /// Matches ignoring `scan_before`. Equals [`Self::total`] when no cursor.
    pub total_without_cursor: usize,
}

/// Ordered index matches for a fully indexed filter: what a cross-workspace
/// merge needs to choose a page before reading any envelope. Counts are those
/// of [`TaskCandidates`].
#[derive(Debug, Default)]
pub struct TaskCandidateKeys {
    pub items: Vec<TaskCandidateKey>,
    /// Matches remaining after `scan_before`, before `limit`.
    pub total: usize,
    /// Matches ignoring `scan_before`. Equals [`Self::total`] when no cursor.
    pub total_without_cursor: usize,
}

/// One selected task with the creation time the listing orders by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCandidateKey {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One task's change stamp: what a change watcher compares between two reads
/// to tell which tasks changed, without reading any envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskChangeStamp {
    pub id: String,
    pub status: TaskStatus,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One fully validated bundle, retaining sidecars from that same read.
#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task: Task,
    pub comments: Vec<TaskComment>,
    pub history: Vec<TaskHistoryEntry>,
    pub artifacts: Vec<ArtifactManifestFileV2>,
}

#[derive(Debug, Default)]
pub struct TaskPage {
    pub items: Vec<TaskRow>,
    /// Matches remaining after `scan_before` (and any residual), before `limit`.
    pub total: usize,
    /// Matches ignoring `scan_before`. Equals [`Self::total`] when no cursor.
    pub total_without_cursor: usize,
    /// Dependency statuses captured after index freshness/rebuild work: every
    /// task in the listed workspace plus each relation target the hydrated
    /// rows name, wherever that target is registered.
    pub status_by_id: std::collections::BTreeMap<String, TaskStatus>,
}

/// Residual application predicate, applied before limiting.
///
/// It must be decidable from envelope metadata: id, title, status, tags,
/// relations, context files and the other envelope fields. The store first
/// evaluates it on a task whose body documents (`description`, `plan`,
/// `execution_summary`, `acceptance_criteria`) are empty, to avoid reading
/// the bundles of candidates it rejects, then again on each hydrated task.
pub type TaskResidualFilter<'a> =
    Option<&'a dyn Fn(&Task, &std::collections::BTreeMap<String, TaskStatus>) -> bool>;
