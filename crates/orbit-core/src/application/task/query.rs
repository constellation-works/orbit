use std::collections::{BTreeMap, BTreeSet};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskStatus,
};

use orbit_store::RegisteredTaskResolution;

use super::listing::{list_task_metadata_in, metadata_task};

use crate::OrbitRuntime;
use crate::application::task::TaskJobPort;

impl OrbitRuntime {
    pub fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "task");
        }
        self.stores()
            .tasks()
            .get_task(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    /// Resolve a dependency target through this machine's registered owner.
    ///
    /// A dependency is the one task reference that legitimately crosses a
    /// workspace boundary: task ids are globally unique, and readiness already
    /// projects statuses across every registered workspace
    /// ([`Self::task_status_index`]). Reading the dependency's body has to
    /// follow the same ownership, or preparation reports a prerequisite that
    /// admission can plainly see as `not found` [ORB-12544].
    ///
    /// Selected-task authority is unchanged: this runtime still acts in, and
    /// writes to, its own workspace only. The resolution never falls back to
    /// another host, never accepts a caller-supplied owner, and never mutates
    /// the prerequisite's workspace.
    pub fn resolve_dependency_task(
        &self,
        id: &str,
    ) -> Result<RegisteredTaskResolution, OrbitError> {
        if self.worker_invocation().is_some() {
            return self
                .read_owner(id, "dependency")
                .map(|task| RegisteredTaskResolution::Resolved(Box::new(task)));
        }
        self.stores().tasks().registered_task(id)
    }

    /// Whether a dependency id names a task this machine can read.
    ///
    /// `false` covers both an id the registered owners do not hold and one
    /// whose prefix belongs to another host; neither is an error, since a
    /// dependency may be recorded before its target exists here.
    pub(crate) fn dependency_task_is_readable(&self, id: &str) -> Result<bool, OrbitError> {
        Ok(matches!(
            self.resolve_dependency_task(id)?,
            RegisteredTaskResolution::Resolved(_)
        ))
    }

    pub fn get_task_artifacts(&self, id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "artifacts");
        }
        self.stores()
            .task_artifacts()
            .get_task_artifacts(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactManifestFileV2>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "manifest");
        }
        self.stores()
            .task_artifacts()
            .get_task_artifact_manifest(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifact(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner_tool_request(
                "orbit.task.artifact.get",
                serde_json::json!({"id": id, "path": path, "_worker_read": "artifact"}),
            );
        }

        self.stores().task_artifacts().get_task_artifact(id, path)
    }

    /// Read one exact artifact through the same task/path authority and inline
    /// bound as the public artifact tool. Missing evidence remains distinguishable.
    pub(crate) fn get_task_artifact_inline(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        orbit_types::task::validate_relative_artifact_path(path)?;
        let task = self.get_task(id)?;
        let artifact = self.get_task_artifact(&task.id, path)?;
        if let Some(artifact) = &artifact {
            artifact.validate_inline_size(&task.id)?;
        }
        Ok(artifact)
    }

    pub fn get_task_comments(&self, id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "comments");
        }
        self.stores()
            .task_history()
            .get_task_comments(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "history");
        }
        self.stores()
            .task_history()
            .get_task_history(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    /// Evidence that a task's history and comments are unchanged since an
    /// earlier stamp, taken without reading them; `None` when nothing can
    /// vouch for them. A worker reads through its owner, so it has none.
    pub(crate) fn task_history_stamp(&self, id: &str) -> Option<String> {
        if self.worker_invocation().is_some() {
            return None;
        }
        self.stores()
            .task_history()
            .task_history_stamp(id)
            .unwrap_or_else(|error| {
                tracing::debug!(task_id = %id, %error, "task history stamp unavailable");
                None
            })
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner("", "tasks");
        }

        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks()
    }

    /// The workspace's tasks as [`Self::list_tasks`] returns them, minus the
    /// body documents (`description`, `acceptance_criteria`, `plan` and
    /// `execution_summary` are empty).
    ///
    /// Reads envelope metadata only, so the cost is one index probe per task
    /// instead of a bundle read. Use it for aggregates that read status,
    /// attribution, tags, priority, type or timestamps; anything that renders
    /// or forwards a task body still needs [`Self::list_tasks`] or
    /// [`Self::get_task`]. A worker invocation reads through its owner, which
    /// only serves whole tasks, so it falls back to the full listing.
    pub fn list_task_metadata(&self) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.list_tasks();
        }

        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        list_task_metadata_in(self.stores().tasks())
    }

    /// [`Self::list_task_metadata`] for the listed `ids` only: one keyed read
    /// per id, so the cost follows the ids asked for, not the workspace. A
    /// worker invocation falls back to the full listing, as it does there.
    pub fn list_task_metadata_for_ids(
        &self,
        ids: &BTreeSet<String>,
    ) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok(self
                .list_tasks()?
                .into_iter()
                .filter(|task| ids.contains(&task.id))
                .collect());
        }

        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        Ok(self
            .stores()
            .tasks()
            .task_envelopes_for_ids(ids)?
            .into_iter()
            .map(metadata_task)
            .collect())
    }

    /// Each backlog task whose effective OS requirement this host does not
    /// satisfy, with the wait: `ORB-12 waits for a macos host (os:macos)`.
    ///
    /// The requirement includes the assigned crew's executor `host_os`, or
    /// the default when no supported pool can run it. A drain reports these,
    /// so one that starts nothing does not read as an empty backlog.
    pub fn host_os_backlog_waits(&self) -> Result<Vec<String>, OrbitError> {
        let limits = self.executor_host_os_limits()?;
        let pools = self.crew_pools_for_input(&serde_json::Value::Null)?;
        let mut waits = Vec::new();
        for task in self.list_task_metadata()? {
            if task.status != orbit_types::task::TaskStatus::Backlog {
                continue;
            }
            let wait = self.task_host_os_wait(&task, &pools, &limits);
            if let Some(wait) = wait {
                waits.push(format!("{} {wait}", task.id));
            }
        }
        Ok(waits)
    }

    /// [`Self::host_os_backlog_waits`] as one drain-start warning, or `None`
    /// when this host can start every backlog task (or the backlog cannot be
    /// read, which the drain itself reports).
    pub fn host_os_backlog_warning(&self) -> Option<String> {
        let waits = self.host_os_backlog_waits().ok()?;
        (!waits.is_empty()).then(|| {
            format!(
                "{} backlog task(s) need a host of another OS and will not start on this {} \
                 host: {}",
                waits.len(),
                self.host_os()
                    .map_or(std::env::consts::OS, orbit_types::task::HostOs::as_str),
                waits.join("; ")
            )
        })
    }

    /// Each backlog task tagged `ui` that waits only for this host's browser
    /// kit: this host satisfies its OS requirement, and its kit is not
    /// verified. Empty once the kit is verified. `orbit doctor` warns with
    /// these, so a host without a kit never leaves its UI work waiting
    /// unexplained.
    pub fn browser_kit_backlog_waits(&self) -> Result<Vec<String>, OrbitError> {
        if self.browser_kit_verified() {
            return Ok(Vec::new());
        }
        let candidates = self
            .list_task_metadata()?
            .into_iter()
            .filter(|task| {
                task.status == orbit_types::task::TaskStatus::Backlog
                    && orbit_types::task::requires_browser_kit(&task.tags)
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let limits = self.executor_host_os_limits()?;
        let pools = self.crew_pools_for_input(&serde_json::Value::Null)?;
        Ok(candidates
            .into_iter()
            .filter(|task| self.task_host_os_wait(task, &pools, &limits).is_none())
            .map(|task| task.id)
            .collect())
    }

    /// Returns the coordination registry's global status projection for
    /// dependency readiness while leaving task listing workspace-scoped.
    pub fn task_status_index(
        &self,
    ) -> Result<BTreeMap<String, orbit_types::task::TaskStatus>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner("", "status_index");
        }

        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_status_index()
    }

    /// The status projection dependency satisfaction reads for `tasks`: the
    /// global index ([`Self::task_status_index`]) with each archived
    /// `blocked_by` target that reached `done` before it was archived
    /// projected as `done` ([`Self::satisfy_completed_archived_dependencies`]).
    pub fn dependency_status_index<'a>(
        &self,
        tasks: impl IntoIterator<Item = &'a Task>,
    ) -> Result<BTreeMap<String, orbit_types::task::TaskStatus>, OrbitError> {
        let mut status_by_id = self.task_status_index()?;
        self.satisfy_completed_archived_dependencies(&mut status_by_id, tasks);
        Ok(status_by_id)
    }

    /// Apply the shared archived-dependency rule
    /// ([`orbit_types::task::satisfy_completed_archived_dependencies`]) to an
    /// existing status projection for `tasks`' dependency edges. Each archived
    /// target's history is read through its registered owner; an unreadable
    /// one keeps its dead end rather than failing the caller.
    pub fn satisfy_completed_archived_dependencies<'a>(
        &self,
        status_by_id: &mut BTreeMap<String, orbit_types::task::TaskStatus>,
        tasks: impl IntoIterator<Item = &'a Task>,
    ) {
        let Ok(()) =
            orbit_types::task::satisfy_completed_archived_dependencies::<std::convert::Infallible>(
                status_by_id,
                tasks.into_iter().flat_map(Task::dependencies),
                |id| {
                    Ok(self.dependency_history(id).unwrap_or_else(|error| {
                        orbit_common::tracing::warn!(
                            task_id = id,
                            %error,
                            "archived dependency history unreadable; keeping it a dead end"
                        );
                        None
                    }))
                },
            );
    }

    /// Status history of a dependency target, read through its registered
    /// owner like [`Self::resolve_dependency_task`].
    pub fn dependency_history(
        &self,
        id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "dependency_history");
        }
        self.stores().tasks().registered_task_history(id)
    }

    /// Status distribution per complexity bucket from the generated task index.
    pub fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<orbit_store::TaskCompletionByComplexity>, OrbitError> {
        if self.worker_invocation().is_some() {
            let rows: Vec<(String, i64, BTreeMap<String, i64>)> =
                self.read_owner("", "completion_by_complexity")?;
            return Ok(rows
                .into_iter()
                .map(
                    |(complexity, total, by_status)| orbit_store::TaskCompletionByComplexity {
                        complexity,
                        total,
                        by_status,
                    },
                )
                .collect());
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_completion_by_complexity()
    }

    /// `task_id →` complexity bucket from the generated task index.
    pub fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner("", "complexity_by_id");
        }

        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_complexity_by_id()
    }

    /// Terminal tasks the generated index dates at or after `since` (all of
    /// them for `None`), oldest first; see
    /// [`orbit_store::TaskTerminalTransition`] for what the time means.
    pub fn task_terminal_transitions(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<orbit_store::TaskTerminalTransition>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner_request(
                serde_json::json!({"_worker_read": "terminal_transitions", "since": since}),
            );
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_terminal_transitions(since)
    }

    /// `bug` tasks the generated index dates (by `created_at`) at or after
    /// `since` (all of them for `None`), oldest first; see
    /// [`orbit_store::TaskDefectFiling`].
    pub fn task_defect_filings(
        &self,
        since: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<Vec<orbit_store::TaskDefectFiling>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner_request(
                serde_json::json!({"_worker_read": "defect_filings", "since": since}),
            );
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_defect_filings(since)
    }

    pub fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self
                .read_owner_request(serde_json::json!({"_worker_read": "tags", "tags": tags}));
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks_by_tags(tags)
    }

    pub fn list_tasks_filtered(
        &self,
        status: Option<orbit_types::task::TaskStatus>,
        priority: Option<orbit_types::task::TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner_request(serde_json::json!({"_worker_read": "filtered", "status": status, "priority": priority, "parent_id": parent_id, "job_run_id": job_run_id, "external_ref": external_ref, "has_external_ref_system": has_external_ref_system}));
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks_filtered(
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    /// The tasks run `run_id` bound on the machine that executes it.
    ///
    /// A run id is unique only within one machine's store, so the owner's own
    /// drain and a follower's claimed leaf can mint the same id in the same
    /// minute [ORB-13649]. A binding records the machine its run executed on,
    /// and that pair is what identifies the run. A claimed leaf asks the owner,
    /// which scopes the read to the leaf's trusted execution machine; a local
    /// run is scoped to the machine its own record names.
    pub fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.list_tasks_filtered(None, None, None, Some(run_id), None, None);
        }
        let executed_on = self.stored_job_run(run_id)?.and_then(|run| run.executed_on);
        let mut tasks = self.list_tasks_filtered(None, None, None, Some(run_id), None, None)?;
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
        Ok(tasks)
    }

    pub fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.search_tasks_filtered(query, &[]);
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().search_tasks(query)
    }

    /// Stream tasks matching `query` to `visit` in listing order until it
    /// returns `false`, as [`Self::search_tasks_filtered`] followed by the
    /// `statuses` filter and `admit` would yield them.
    ///
    /// The store selects `statuses` from its index, so a task in another
    /// status costs no read. `admit` judges a task from its envelope alone,
    /// so the store skips the body read for every task it rejects, and
    /// stopping early skips the rest. A worker invocation reads through its
    /// owner, which answers whole searches only, so it filters that answer
    /// instead.
    pub(crate) fn search_tasks_visit(
        &self,
        query: &str,
        tags: &[String],
        statuses: Option<&[TaskStatus]>,
        admit: &dyn Fn(&Task) -> bool,
        visit: &mut dyn FnMut(Task) -> bool,
    ) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            for task in self.search_tasks_filtered(query, tags)? {
                if statuses.is_none_or(|statuses| statuses.contains(&task.status))
                    && admit(&task)
                    && !visit(task)
                {
                    break;
                }
            }
            return Ok(());
        }
        if !self.coordination_task_reads_visible() {
            return Ok(());
        }
        self.stores()
            .tasks()
            .search_tasks_visit(query, tags, statuses, admit, visit)
    }

    /// Run `op` as one read section of this workspace's task store: the task
    /// reads it makes on this thread share one acquisition of the commit
    /// boundary instead of each taking it
    /// ([`orbit_store::TaskStoreBackend::in_read_section`]). For a read-only
    /// surface whose reads are many and short; `op` must not admit work or
    /// wait on another thread's task reads. A worker reads through its owner
    /// and a replica checkout reads no local tasks, so both run `op` directly.
    pub(crate) fn in_task_read_section<T>(
        &self,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        if self.worker_invocation().is_some() || !self.coordination_task_reads_visible() {
            return op();
        }
        let mut op = Some(op);
        let mut output = None;
        self.stores().tasks().in_read_section(&mut || {
            if let Some(op) = op.take() {
                output = Some(op()?);
            }
            Ok(())
        })?;
        output
            .ok_or_else(|| OrbitError::Store("task read section did not run its operation".into()))
    }

    pub fn search_tasks_filtered(
        &self,
        query: &str,
        tags: &[String],
    ) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner_request(
                serde_json::json!({"_worker_read": "search", "query": query, "tags": tags}),
            );
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().search_tasks_filtered(query, tags)
    }
}
