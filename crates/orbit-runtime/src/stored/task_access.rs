//! The task access [`Stored`] is built with: this checkout's own store, or
//! the owner route for a claimed worker. Replica checkouts hide coordination
//! listings. Both workers and replicas refuse local coordination writes.
//!
//! The route is chosen once from the process's [`Role`]. A task read calls
//! [`Stored::task`] and does not ask which kind of process it runs in. Each
//! read names both of its paths: the local store call, and the owner
//! projection a worker's read is answered from.

mod metadata;
mod owner;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contract::{TaskCandidates, TaskLifecycleStamp, TaskListFilter, TaskRow};
use orbit_store::{
    RegisteredTaskResolution, TaskCompletionByComplexity, TaskDefectFiling, TaskTerminalTransition,
};
use orbit_tools::OwnerCoordinator;
use orbit_types::task::{
    ArtifactManifestFileV2, ExecutionLocation, ExternalRef, Task, TaskArtifact, TaskComment,
    TaskHistoryEntry, TaskPriority, TaskStatus,
};
use serde_json::{Value, json};

use super::Stored;
use super::stores::OrbitStores;
use crate::RuntimeKernel;
use crate::base::role::{CoordinationAccess, Role};

pub use self::metadata::{list_task_metadata_in, list_task_metadata_matching, metadata_task};
pub(crate) use self::owner::OwnerRoute;
#[cfg(feature = "test-support")]
pub use self::owner::with_owner_read_retry_wait;
pub(crate) use self::owner::worker_binding_missing;

/// The coordination access bound to the worker's actual owner route, if any.
pub(crate) type TaskRoute = CoordinationAccess<OwnerRoute>;

impl TaskRoute {
    pub(crate) fn for_role(role: &Role, coordinator: Option<Arc<dyn OwnerCoordinator>>) -> Self {
        role.coordination(|invocation| OwnerRoute::new(Arc::clone(invocation), coordinator))
    }

    pub(crate) fn owner(&self) -> Option<&OwnerRoute> {
        match self {
            Self::Claimed(route) => Some(route),
            Self::Local | Self::Replica(_) => None,
        }
    }

    pub(crate) fn owner_mut(&mut self) -> Option<&mut OwnerRoute> {
        match self {
            Self::Claimed(route) => Some(route),
            Self::Local | Self::Replica(_) => None,
        }
    }
}

impl RuntimeKernel {
    /// Refuse control-plane work in a replica checkout or claimed worker.
    pub fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError> {
        self.stored().task().writer().map(|_| ())
    }
}

/// One worker's reads through its owner route.
struct OwnerRead<'a> {
    route: &'a OwnerRoute,
    /// This worker's repository root.
    checkout: &'a Path,
}

impl OwnerRead<'_> {
    /// The named projection of task `id`.
    fn projection<T: serde::de::DeserializeOwned>(
        &self,
        id: &str,
        projection: &str,
    ) -> Result<T, OrbitError> {
        self.request(json!({"id": id, "_worker_read": projection}))
    }

    fn request<T: serde::de::DeserializeOwned>(&self, request: Value) -> Result<T, OrbitError> {
        self.tool_request("orbit.task.show", request)
    }

    fn tool_request<T: serde::de::DeserializeOwned>(
        &self,
        tool: &str,
        request: Value,
    ) -> Result<T, OrbitError> {
        self.route.read(self.checkout, tool, request)
    }

    /// The owner answers `Option<Task>`: `null` for a dangling or foreign
    /// prerequisite. An older owner instead refuses that case with `owner
    /// dependency unavailable`; treat that exact refusal as the same missing
    /// answer rather than a failure.
    fn dependency(&self, id: &str) -> Result<Option<Task>, OrbitError> {
        match self.projection(id, "dependency") {
            Ok(task) => Ok(task),
            Err(error) if is_legacy_dependency_unavailable(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// Whether `error` is an older owner's refusal of an unavailable dependency
/// projection, which a current owner answers as `null` instead.
fn is_legacy_dependency_unavailable(error: &OrbitError) -> bool {
    let text = error.to_string();
    text.contains("owner dependency unavailable")
}

fn task_not_found(id: &str) -> OrbitError {
    OrbitError::not_found(NotFoundKind::Task, id.to_string())
}

enum Chosen<'a> {
    Local(&'a OrbitStores),
    Owner(OwnerRead<'a>),
}

/// The lightweight listed row of a task whose bundle could not be read.
pub enum ListedRow {
    /// The row.
    Row(Box<TaskRow>),
    /// No listed row describes the task.
    Absent,
    /// The owner did not serve the projection, with its error.
    Unserved(OrbitError),
}

/// Which machine's bindings a run's task list keeps; see
/// [`TaskAccess::run_task_scope`].
pub enum RunTaskScope {
    /// The owner already scoped the read to the worker's trusted execution
    /// machine.
    ScopedByOwner,
    /// The machine the run's own record names, when it names one.
    ExecutedOn(Option<ExecutionLocation>),
}

impl RunTaskScope {
    /// Drop the tasks another machine's run of the same id bound.
    pub fn narrow(&self, tasks: &mut Vec<Task>) {
        let Self::ExecutedOn(executed_on) = self else {
            return;
        };
        tasks.retain(|task| match &task.job_run_machine {
            Some(bound) => executed_on
                .as_ref()
                .is_some_and(|local| local.machine_id == bound.machine_id),
            // Only a claim binds a task for another machine, and a claim always
            // records that machine. An unrecorded location is a local binding
            // made by a run without a machine identity or before locations
            // were recorded.
            None => true,
        });
    }
}

/// This process's task reads and coordination writes: [`Stored::task`].
#[derive(Clone, Copy)]
pub struct TaskAccess<'a> {
    stored: &'a Stored,
}

impl<'a> TaskAccess<'a> {
    pub(super) fn new(stored: &'a Stored) -> Self {
        Self { stored }
    }

    /// Whether this checkout exposes coordination task listings and projections.
    pub fn reads_visible(&self) -> bool {
        self.stored.task_route.reads_visible()
    }

    /// Whether the drain owner holds this process's task records, as it does
    /// for a claimed worker. Reads are then answered from the owner's
    /// projections, and nothing local describes a task or takes a write for
    /// one: not the index, a read stamp, the history or a task lock.
    pub fn held_by_owner(&self) -> bool {
        self.stored.task_route.owner().is_some()
    }

    /// The local task store for a coordination write, or this checkout's refusal.
    pub fn writer(&self) -> Result<&'a orbit_store::TaskV2Store, OrbitError> {
        self.stored.task_route.ensure_write()?;
        Ok(self.stored.stores.tasks())
    }

    fn visible_read(&self) -> Option<Chosen<'a>> {
        match &self.stored.task_route {
            TaskRoute::Local => Some(Chosen::Local(&self.stored.stores)),
            TaskRoute::Replica(_) => None,
            TaskRoute::Claimed(route) => Some(Chosen::Owner(OwnerRead {
                route,
                checkout: &self.stored.base.paths().repo_root,
            })),
        }
    }

    /// Direct task and history reads also use the local store on a replica.
    fn chosen(&self) -> Chosen<'a> {
        self.visible_read()
            .unwrap_or(Chosen::Local(&self.stored.stores))
    }

    /// One task. Local: the store's `get_task`. Owner: the `task` projection.
    pub fn get(&self, id: &str) -> Result<Task, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.projection(id, "task"),
            Chosen::Local(stores) => stores
                .tasks()
                .get_task(id)?
                .ok_or_else(|| task_not_found(id)),
        }
    }

    /// One task with its comments, history and artifact manifest; when the
    /// task itself cannot be read, `unreadable` decides from that error.
    /// Local: the store's `get_task_row`. Owner: the `task`, `comments`,
    /// `history` and `manifest` projections, the last three failing the read.
    pub fn row_or(
        &self,
        id: &str,
        unreadable: impl FnOnce(OrbitError) -> Result<TaskRow, OrbitError>,
    ) -> Result<TaskRow, OrbitError> {
        let task = match self.chosen() {
            Chosen::Owner(_) => self.get(id),
            Chosen::Local(stores) => {
                return match stores.tasks().get_task_row(id, false) {
                    Ok(Some(row)) => Ok(row),
                    Ok(None) => unreadable(task_not_found(id)),
                    Err(error) => unreadable(error),
                };
            }
        };
        match task {
            Ok(task) => Ok(TaskRow {
                task,
                comments: self.comments(id)?,
                history: self.history(id)?,
                artifacts: self.artifact_manifest(id)?,
            }),
            Err(error) => unreadable(error),
        }
    }

    /// The listed row of a task whose bundle could not be read. Local: the
    /// store's listed `get_task_row`. Owner: the `listed_task` projection,
    /// `null` when the owner lists none.
    pub fn listed_row(&self, id: &str) -> Result<ListedRow, OrbitError> {
        match self.visible_read() {
            None => Ok(ListedRow::Absent),
            Some(Chosen::Owner(owner)) => match owner.projection::<Value>(id, "listed_task") {
                Ok(Value::Null) => Ok(ListedRow::Absent),
                Ok(value) => serde_json::from_value(value)
                    .map(|row| ListedRow::Row(Box::new(row)))
                    .map_err(|decode| {
                        OrbitError::Store(format!("worker listed task row: {decode}"))
                    }),
                Err(error) => Ok(ListedRow::Unserved(error)),
            },
            Some(Chosen::Local(stores)) => Ok(match stores.tasks().get_task_row(id, true)? {
                Some(row) => ListedRow::Row(Box::new(row)),
                None => ListedRow::Absent,
            }),
        }
    }

    /// A dependency target through this machine's registered owner. Local:
    /// the store's `registered_task`. Owner: the `dependency` projection,
    /// which answers a dangling or foreign id as missing.
    pub fn resolve_dependency(&self, id: &str) -> Result<RegisteredTaskResolution, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => Ok(match owner.dependency(id)? {
                Some(task) => RegisteredTaskResolution::Resolved(Box::new(task)),
                None => RegisteredTaskResolution::Missing,
            }),
            Chosen::Local(stores) => stores.tasks().registered_task(id),
        }
    }

    /// The workspace partition holding `id`. Local: the store's
    /// `registered_task_workspace`. Owner: the `dependency` projection.
    pub fn registered_workspace(&self, id: &str) -> Result<Option<String>, OrbitError> {
        match self.chosen() {
            // The owner read carries no partition; the binding's workspace
            // is the workspace the owner route serves this call for.
            Chosen::Owner(owner) => Ok(owner
                .dependency(id)?
                .map(|_| owner.route.invocation().owner_workspace_id.clone())),
            Chosen::Local(stores) => stores.tasks().registered_task_workspace(id),
        }
    }

    /// Status history of a dependency target. Local: the store's
    /// `registered_task_history`. Owner: the `dependency_history` projection.
    pub fn dependency_history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.projection(id, "dependency_history"),
            Chosen::Local(stores) => stores.tasks().registered_task_history(id),
        }
    }

    /// Local: the artifact store's `get_task_artifacts`. Owner: the
    /// `artifacts` projection.
    pub fn artifacts(&self, id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.projection(id, "artifacts"),
            Chosen::Local(stores) => stores
                .task_artifacts()
                .get_task_artifacts(id)?
                .ok_or_else(|| task_not_found(id)),
        }
    }

    /// Local: the artifact store's `get_task_artifact_manifest`. Owner: the
    /// `manifest` projection.
    pub fn artifact_manifest(&self, id: &str) -> Result<Vec<ArtifactManifestFileV2>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.projection(id, "manifest"),
            Chosen::Local(stores) => stores
                .task_artifacts()
                .get_task_artifact_manifest(id)?
                .ok_or_else(|| task_not_found(id)),
        }
    }

    /// Local: the artifact store's `get_task_artifact`. Owner: the `artifact`
    /// projection of `orbit.task.artifact.get`.
    pub fn artifact(&self, id: &str, path: &str) -> Result<Option<TaskArtifact>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.tool_request(
                "orbit.task.artifact.get",
                json!({"id": id, "path": path, "_worker_read": "artifact"}),
            ),
            Chosen::Local(stores) => stores.task_artifacts().get_task_artifact(id, path),
        }
    }

    /// Lowercase hex SHA-256 of a task artifact's stored bytes (the bytes
    /// `orbit.task.artifact.get` returns); `None` when the task has no
    /// artifact at `path`.
    pub fn artifact_sha256(&self, id: &str, path: &str) -> Result<Option<String>, OrbitError> {
        Ok(self
            .artifact(id, path)?
            .map(|artifact| orbit_common::security::release::sha256_hex(&artifact.content)))
    }

    /// One exact artifact through the same task and path authority, and the
    /// same inline size bound, as the public artifact tool. A missing artifact
    /// stays distinguishable from a refused one.
    pub fn artifact_inline(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        orbit_types::task::validate_relative_artifact_path(path)?;
        let task = self.get(id)?;
        let artifact = self.artifact(&task.id, path)?;
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(&task.id)?;
        }
        Ok(artifact)
    }

    /// Local: the history store's `get_task_comments`. Owner: the `comments`
    /// projection.
    pub fn comments(&self, id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.request(json!({
                "id": id,
                "_worker_read": "comments",
                // Older owners ignore this hint and return inline audits.
                // Other tasks' artifacts are outside a worker's claim, so
                // keep their audits expanded at the owner.
                "pilot_artifacts": id == owner.route.invocation().task_id,
            })),
            Chosen::Local(stores) => stores
                .task_history()
                .get_task_comments(id)?
                .ok_or_else(|| task_not_found(id)),
        }
    }

    /// Local: the history store's `get_task_history`. Owner: the `history`
    /// projection.
    pub fn history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => owner.projection(id, "history"),
            Chosen::Local(stores) => stores
                .task_history()
                .get_task_history(id)?
                .ok_or_else(|| task_not_found(id)),
        }
    }

    /// The history of each listed task in request order. Local: one batch
    /// read, checking partition settledness once. Owner: one `history`
    /// projection per task.
    pub fn histories(&self, ids: &[String]) -> Vec<Result<Vec<TaskHistoryEntry>, OrbitError>> {
        match self.chosen() {
            Chosen::Owner(_) => ids.iter().map(|id| self.history(id)).collect(),
            Chosen::Local(stores) => {
                let rows = stores.task_history().get_task_histories(ids);
                ids.iter()
                    .zip(rows)
                    .map(|(id, row)| match row {
                        Ok(Some(history)) => Ok(history),
                        Ok(None) => Err(task_not_found(id)),
                        Err(error) => Err(error),
                    })
                    .collect()
            }
        }
    }

    /// Evidence that each listed task's history and comments are unchanged.
    /// Local: the history store's stamps. Owner: none, since nothing local
    /// can vouch for a log the owner holds.
    pub fn history_stamps(&self, ids: &[String]) -> Result<BTreeMap<String, String>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(_) => Ok(BTreeMap::new()),
            Chosen::Local(stores) => stores.task_history().task_history_stamps(ids),
        }
    }

    /// Evidence that each listed task's whole bundle is unchanged. Local: the
    /// history store's bundle stamps. Owner: none.
    pub fn bundle_stamps(&self, ids: &[String]) -> Result<BTreeMap<String, String>, OrbitError> {
        match self.chosen() {
            Chosen::Owner(_) => Ok(BTreeMap::new()),
            Chosen::Local(stores) => stores.task_history().task_bundle_stamps(ids),
        }
    }

    /// Local: the store's `list_tasks`. Owner: the `tasks` projection.
    pub fn list(&self) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => owner.projection("", "tasks"),
            Some(Chosen::Local(stores)) => stores.tasks().list_tasks(),
        }
    }

    /// Every task without its body documents. Local: the index. Owner:
    /// complete bounded `task_metadata` pages; any failed page fails the read.
    pub fn metadata(&self) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => metadata::collect_owner_metadata(&owner, None),
            Some(Chosen::Local(stores)) => list_task_metadata_in(stores.tasks()),
        }
    }

    /// [`Self::metadata`] narrowed to the tasks carrying at least one of
    /// `tags`. Local: the index selects them. Owner: every `task_metadata`
    /// page, filtered here.
    pub fn metadata_with_any_tag(&self, tags: &BTreeSet<String>) -> Result<Vec<Task>, OrbitError> {
        let any_tags: Vec<String> = tags.iter().cloned().collect();
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                let mut tasks = metadata::collect_owner_metadata(&owner, None)?;
                let normalized = orbit_types::task::normalize_task_tags(any_tags);
                tasks.retain(|task| {
                    task.tags
                        .iter()
                        .any(|tag| normalized.contains(&tag.trim().to_lowercase()))
                });
                Ok(tasks)
            }
            Some(Chosen::Local(stores)) => list_task_metadata_matching(
                stores.tasks(),
                &TaskListFilter {
                    any_tags,
                    ..TaskListFilter::default()
                },
            ),
        }
    }

    /// Metadata covering what `filter` selects, for a caller that applies its
    /// own predicate to the answer. Local: exactly the filter's selection,
    /// from the index. Owner: every task, since the owner answers whole
    /// `task_metadata` scans only.
    pub fn metadata_covering(&self, filter: &TaskListFilter) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => metadata::collect_owner_metadata(&owner, None),
            Some(Chosen::Local(stores)) => list_task_metadata_matching(stores.tasks(), filter),
        }
    }

    /// [`Self::metadata`] for the listed `ids` only. Local: one keyed envelope
    /// read per id. Owner: bounded `task_metadata` ID selections.
    pub fn metadata_for_ids(&self, ids: &BTreeSet<String>) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => metadata::collect_owner_metadata(&owner, Some(ids)),
            Some(Chosen::Local(stores)) => Ok(stores
                .tasks()
                .task_envelopes_for_ids(ids)?
                .into_iter()
                .map(metadata_task)
                .collect()),
        }
    }

    /// Creation time and status of the tasks in `statuses` or updated at or
    /// after `updated_since`. Local: the store's validated index. Owner:
    /// [`Self::metadata`], filtered here.
    pub fn lifecycle_stamps_matching(
        &self,
        statuses: &[TaskStatus],
        updated_since: DateTime<Utc>,
    ) -> Result<Vec<TaskLifecycleStamp>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(_)) => Ok(self
                .metadata()?
                .into_iter()
                .filter(|task| statuses.contains(&task.status) || task.updated_at >= updated_since)
                .map(|task| TaskLifecycleStamp {
                    id: task.id,
                    status: task.status,
                    created_at: task.created_at,
                })
                .collect()),
            Some(Chosen::Local(stores)) => stores
                .tasks()
                .task_lifecycle_stamps_matching(statuses, updated_since),
        }
    }

    /// Local: the store's `task_status_index`. Owner: the `status_index`
    /// projection.
    pub fn status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        match self.visible_read() {
            None => Ok(BTreeMap::new()),
            Some(Chosen::Owner(owner)) => owner.projection("", "status_index"),
            Some(Chosen::Local(stores)) => stores.tasks().task_status_index(),
        }
    }

    /// Local: the store's `task_completion_by_complexity`. Owner: the
    /// `completion_by_complexity` projection.
    pub fn completion_by_complexity(&self) -> Result<Vec<TaskCompletionByComplexity>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                let rows: Vec<(String, i64, BTreeMap<String, i64>)> =
                    owner.projection("", "completion_by_complexity")?;
                Ok(rows
                    .into_iter()
                    .map(
                        |(complexity, total, by_status)| TaskCompletionByComplexity {
                            complexity,
                            total,
                            by_status,
                        },
                    )
                    .collect())
            }
            Some(Chosen::Local(stores)) => stores.tasks().task_completion_by_complexity(),
        }
    }

    /// Local: the store's `task_complexity_by_id`. Owner: the
    /// `complexity_by_id` projection.
    pub fn complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        match self.visible_read() {
            None => Ok(BTreeMap::new()),
            Some(Chosen::Owner(owner)) => owner.projection("", "complexity_by_id"),
            Some(Chosen::Local(stores)) => stores.tasks().task_complexity_by_id(),
        }
    }

    /// Local: the store's `task_terminal_transitions`. Owner: the
    /// `terminal_transitions` projection.
    pub fn terminal_transitions(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<TaskTerminalTransition>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                owner.request(json!({"_worker_read": "terminal_transitions", "since": since}))
            }
            Some(Chosen::Local(stores)) => stores.tasks().task_terminal_transitions(since),
        }
    }

    /// Local: the store's `task_defect_filings`. Owner: the `defect_filings`
    /// projection.
    pub fn defect_filings(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<TaskDefectFiling>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                owner.request(json!({"_worker_read": "defect_filings", "since": since}))
            }
            Some(Chosen::Local(stores)) => stores.tasks().task_defect_filings(since),
        }
    }

    /// Local: the store's `list_tasks_by_tags`. Owner: the `tags` projection.
    pub fn list_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                owner.request(json!({"_worker_read": "tags", "tags": tags}))
            }
            Some(Chosen::Local(stores)) => stores.tasks().list_tasks_by_tags(tags),
        }
    }

    /// Local: the store's `list_tasks_filtered`. Owner: the `filtered`
    /// projection.
    pub fn list_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => owner.request(json!({"_worker_read": "filtered", "status": status, "priority": priority, "parent_id": parent_id, "job_run_id": job_run_id, "external_ref": external_ref, "has_external_ref_system": has_external_ref_system})),
            Some(Chosen::Local(stores)) => stores.tasks().list_tasks_filtered(
                status,
                priority,
                parent_id,
                job_run_id,
                external_ref,
                has_external_ref_system,
            ),
        }
    }

    /// The admission candidates the local store ranks for `filter`, or none
    /// where this checkout exposes no coordination listing.
    pub fn candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        if !self.reads_visible() {
            return Ok(TaskCandidates::default());
        }
        self.stored.stores.tasks().task_candidates(filter, limit)
    }

    /// Which machine's bindings the task list of run `run_id` keeps. Local:
    /// the machine the run's own stored record names. Owner: nothing is read,
    /// since the owner scopes the `filtered` projection to the worker's
    /// trusted execution machine.
    pub fn run_task_scope(&self, run_id: &str) -> Result<RunTaskScope, OrbitError> {
        match self.chosen() {
            Chosen::Owner(_) => Ok(RunTaskScope::ScopedByOwner),
            Chosen::Local(stores) => Ok(RunTaskScope::ExecutedOn(
                stores
                    .jobs()
                    .get_job_run(run_id)?
                    .and_then(|run| run.executed_on),
            )),
        }
    }

    /// Local: the store's `search_tasks`. Owner: the `search` projection with
    /// no tags.
    pub fn search(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(_)) => self.search_filtered(query, &[]),
            Some(Chosen::Local(stores)) => stores.tasks().search_tasks(query),
        }
    }

    /// Local: the store's `search_tasks_filtered`. Owner: the `search`
    /// projection.
    pub fn search_filtered(&self, query: &str, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        match self.visible_read() {
            None => Ok(Vec::new()),
            Some(Chosen::Owner(owner)) => {
                owner.request(json!({"_worker_read": "search", "query": query, "tags": tags}))
            }
            Some(Chosen::Local(stores)) => stores.tasks().search_tasks_filtered(query, tags),
        }
    }

    /// Stream the tasks matching `query` to `visit` until it returns `false`.
    /// Local: the store's `search_tasks_visit`, which skips the body read of
    /// every task `statuses` or `admit` rejects. Owner: the whole `search`
    /// projection, filtered here, since the owner answers whole searches only.
    pub fn search_visit(
        &self,
        query: &str,
        tags: &[String],
        statuses: Option<&[TaskStatus]>,
        admit: &dyn Fn(&Task) -> bool,
        visit: &mut dyn FnMut(Task) -> bool,
    ) -> Result<(), OrbitError> {
        match self.visible_read() {
            None => Ok(()),
            Some(Chosen::Owner(_)) => {
                for task in self.search_filtered(query, tags)? {
                    if statuses.is_none_or(|statuses| statuses.contains(&task.status))
                        && admit(&task)
                        && !visit(task)
                    {
                        break;
                    }
                }
                Ok(())
            }
            Some(Chosen::Local(stores)) => stores
                .tasks()
                .search_tasks_visit(query, tags, statuses, admit, visit),
        }
    }

    /// Run `op` as one read section. Local: the store's `in_read_section`,
    /// so the task reads `op` makes on this thread share one acquisition of
    /// the commit boundary. Owner: `op` runs directly.
    pub fn in_read_section<T>(
        &self,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let stores = match self.visible_read() {
            None | Some(Chosen::Owner(_)) => return op(),
            Some(Chosen::Local(stores)) => stores,
        };
        let mut op = Some(op);
        let mut output = None;
        stores.tasks().in_read_section(&mut || {
            if let Some(op) = op.take() {
                output = Some(op()?);
            }
            Ok(())
        })?;
        output
            .ok_or_else(|| OrbitError::Store("task read section did not run its operation".into()))
    }

    /// The steering records the owner holds for a claimed task. Owner: the
    /// `steering` projection. Local: refused; only a claimed worker has
    /// claim steering to read.
    pub fn claim_steering<T: serde::de::DeserializeOwned>(
        &self,
        task_id: &str,
    ) -> Result<T, OrbitError> {
        match self.chosen() {
            Chosen::Owner(owner) => {
                owner.request(json!({"id": task_id, "_worker_read": "steering"}))
            }
            Chosen::Local(_) => Err(worker_binding_missing()),
        }
    }
}
