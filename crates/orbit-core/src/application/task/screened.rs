//! Task add, update and approval as an operator or agent surface performs
//! them: the context-selector screening and every advisory warning run here,
//! once, so the CLI, the `orbit.task.*` tools and the dashboard only parse
//! their input and render the result.
//!
//! Internal callers keep the permissive writes ([`OrbitRuntime::add_task`],
//! [`OrbitRuntime::update_task_with_identity`]): task-pilot apply, automation
//! seeding and runtime host updates legitimately record targets a task is
//! about to create, and they have no one to warn.

use orbit_common::OrbitError;
use orbit_types::task::{Task, backlog_footprint_warning};

use crate::OrbitRuntime;
use crate::runtime::run_input::managed_run_context_run_id_from_env;

use super::params::{TaskAddParams, TaskUpdateParams};
use super::paths::{ContextCreationAuthorization, compute_task_add_warnings};

/// How a surface's task write screens its `context_files`, and the managed
/// run its caller is bound to.
///
/// The binding comes only from a surface's trusted run envelope, never from
/// request input, so the constructors below are the only way to set it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskWriteScreen {
    allow_missing_context: bool,
    owner_run_id: Option<String>,
}

impl TaskWriteScreen {
    /// A caller no managed run vouches for, such as a dashboard request: the
    /// HTTP client is not the server process, so the server's own
    /// environment says nothing about who is calling.
    pub fn unbound(allow_missing_context: bool) -> Self {
        Self {
            allow_missing_context,
            owner_run_id: None,
        }
    }

    /// A caller running in this process, bound to the managed run Orbit
    /// stamped into its environment when it spawned it (the same envelope
    /// `orbit tool run` trusts). Outside a managed run it is unbound.
    pub fn from_process(allow_missing_context: bool) -> Self {
        Self::bound(allow_missing_context, managed_run_context_run_id_from_env())
    }

    /// A caller bound to `owner_run_id` by the tool host's dispatch.
    pub(crate) fn bound(allow_missing_context: bool, owner_run_id: Option<String>) -> Self {
        Self {
            allow_missing_context,
            owner_run_id,
        }
    }

    pub(crate) fn owner_run_id(&self) -> Option<&str> {
        self.owner_run_id.as_deref()
    }
}

/// A screened task write: the stored task and what the write established
/// for its surface to render.
#[derive(Debug, Clone)]
pub struct ScreenedTaskWrite {
    pub task: Task,
    /// Advisory signals about the stored task. None of them refused the write.
    pub warnings: Vec<String>,
    /// Canonical selectors stored without a verified anchor because the
    /// task's owning run declared them from its worktree. Empty for every
    /// strict write and for an explicit `allow_missing_context`.
    pub context_files_unverified: Vec<String>,
}

impl From<Task> for ScreenedTaskWrite {
    /// A write with nothing to screen or warn about, such as a comment.
    fn from(task: Task) -> Self {
        Self {
            task,
            warnings: Vec::new(),
            context_files_unverified: Vec::new(),
        }
    }
}

/// Which lifecycle rule a surface's status change is held to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskUpdateAuthority {
    /// The lifecycle table decides.
    #[default]
    Lifecycle,
    /// A human overrides the table (`orbit task update --force`, the
    /// dashboard's `force`); the tool surface never offers it.
    Forced,
}

impl OrbitRuntime {
    /// Create a task for an operator or agent surface.
    ///
    /// Every selector must resolve unless the screen allows missing context,
    /// which records each missing one as a durable creation grant.
    pub fn add_task_screened(
        &self,
        mut params: TaskAddParams,
        screen: &TaskWriteScreen,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        params.context_creation = if screen.allow_missing_context {
            self.authorize_missing_context(&params.context_files)?
        } else {
            self.ensure_context_selectors_exist(&params.context_files)?;
            ContextCreationAuthorization::default()
        };
        let parent_warning = params
            .parent_id
            .as_deref()
            .filter(|parent_id| self.get_task(parent_id).is_err())
            .map(|parent_id| {
                format!("parent task '{parent_id}' was not found; creating subtask anyway")
            });
        let dependency_warnings = self.unreadable_dependency_warnings(&params.dependencies);
        let requested_context_files = params.context_files.clone();

        let (task, mut warnings) = self.add_task_reporting(params, agent, model)?;
        warnings.extend(parent_warning);
        warnings.extend(dependency_warnings);
        warnings.extend(compute_task_add_warnings(
            &requested_context_files,
            task.task_type,
        ));
        // [ORB-15191] Filed straight into the backlog with no footprint, a
        // live drain admits it before a pilot can prepare one.
        warnings.extend(backlog_footprint_warning(&task));
        Ok(ScreenedTaskWrite {
            task,
            warnings,
            context_files_unverified: Vec::new(),
        })
    }

    /// Update a task for an operator or agent surface, under `authority`.
    pub fn update_task_screened(
        &self,
        id: &str,
        params: TaskUpdateParams,
        screen: &TaskWriteScreen,
        authority: TaskUpdateAuthority,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        self.write_screened_update(id, params, screen, |params| match authority {
            TaskUpdateAuthority::Lifecycle => {
                self.update_task_with_identity(id, params, agent, model)
            }
            TaskUpdateAuthority::Forced => {
                self.force_update_task_with_identity(id, params, agent, model)
            }
        })
    }

    /// The `orbit.task.update` write: lifecycle-checked, with the status
    /// `note`, and owned by the caller's run for artifact attribution.
    pub(crate) fn update_task_screened_for_tool(
        &self,
        id: &str,
        params: TaskUpdateParams,
        screen: &TaskWriteScreen,
        agent: Option<String>,
        model: Option<String>,
        note: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        let owner = screen.owner_run_id().map(ToOwned::to_owned);
        self.write_screened_update(id, params, screen, |params| {
            self.update_task_with_owner(id, params, agent, model, note, owner)
        })
    }

    /// The `orbit.task.update` move into `in-progress`, which starts work
    /// and absorbs the write's field edits.
    pub(crate) fn start_task_screened(
        &self,
        id: &str,
        params: TaskUpdateParams,
        screen: &TaskWriteScreen,
        note: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        let owner = screen.owner_run_id().map(ToOwned::to_owned);
        self.write_screened_update(id, params, screen, |params| {
            self.start_task_with_identity_and_crew(
                id,
                note,
                params.comment.clone(),
                agent,
                model,
                params.crew.clone().flatten(),
                params.plan.clone(),
                params,
                owner,
            )
        })
    }

    /// Take a task's next approval step for an operator surface, warning
    /// with the blockers its latest task-pilot assessment recorded.
    pub fn approve_task_screened(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        let warnings = self.approval_blocker_warnings(id)?;
        let task = self.approve_task_with_identity(id, note, comment, agent, model)?;
        Ok(Self::approval_write(task, warnings))
    }

    /// The `orbit.task.update` approval (`status: backlog` on a proposed
    /// task), with the same warnings as [`Self::approve_task_screened`].
    pub(crate) fn transition_task_to_backlog_screened(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        let warnings = self.approval_blocker_warnings(id)?;
        let task =
            self.transition_task_to_backlog_with_identity(id, note, comment, agent, model)?;
        Ok(Self::approval_write(task, warnings))
    }

    /// Screen an update's replacement selectors, write it with `write`, and
    /// collect its warnings.
    ///
    /// With `allow_missing_context` every missing selector becomes a durable
    /// creation grant. Otherwise the strict check runs, keeping a selector
    /// the task already holds a grant for and relaxing a missing anchor for
    /// the task's owning run calling from its worktree; those are reported
    /// as unverified.
    fn write_screened_update(
        &self,
        id: &str,
        mut params: TaskUpdateParams,
        screen: &TaskWriteScreen,
        write: impl FnOnce(TaskUpdateParams) -> Result<Task, OrbitError>,
    ) -> Result<ScreenedTaskWrite, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut context_files_unverified = Vec::new();
        if let Some(candidates) = params.context_files.as_deref() {
            params.context_creation = if screen.allow_missing_context {
                self.authorize_missing_context(candidates)?
            } else {
                let (unverified, authorization) = self
                    .ensure_context_selectors_exist_for_task_write(
                        id,
                        screen.owner_run_id(),
                        candidates,
                    )?;
                context_files_unverified = unverified;
                authorization
            };
        }
        let mut warnings = params
            .dependencies
            .as_deref()
            .map(|dependencies| self.unreadable_dependency_warnings(dependencies))
            .unwrap_or_default();

        let task = write(params)?;
        // [ORB-15191] A write that leaves the task in the backlog with no
        // footprint says so as loudly as an add does.
        warnings.extend(backlog_footprint_warning(&task));
        Ok(ScreenedTaskWrite {
            task,
            warnings,
            context_files_unverified,
        })
    }

    /// A dependency may legitimately be recorded before its target exists
    /// here (another workspace or host owns it), so an unreadable one is
    /// stored and only warned about, which still surfaces a typo.
    fn unreadable_dependency_warnings(&self, dependencies: &[String]) -> Vec<String> {
        dependencies
            .iter()
            .filter(|id| {
                self.dependency_task_is_readable(id)
                    .is_ok_and(|readable| !readable)
            })
            .map(|id| format!("dependency task '{id}' was not found; recording it anyway"))
            .collect()
    }

    fn approval_blocker_warnings(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        Ok(self
            .task_pilot_blocker_warnings(id)?
            .into_iter()
            .map(|text| format!("Task-pilot blocker: {text}"))
            .collect())
    }

    fn approval_write(task: Task, mut warnings: Vec<String>) -> ScreenedTaskWrite {
        warnings.extend(backlog_footprint_warning(&task));
        ScreenedTaskWrite {
            task,
            warnings,
            context_files_unverified: Vec::new(),
        }
    }
}
