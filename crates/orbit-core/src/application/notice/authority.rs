//! Who is posting, and what that poster may do.
//!
//! The poster class comes from the caller's resolved capabilities and its run
//! binding, never from input. Orchestrator attribution is self-declared (the
//! same trust task filing extends), so the rights it unlocks are the ones a
//! mistaken unmanaged session could use without lasting harm.

use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_types::record::{AuthorClass, NoticeAuthor, NoticeKind, NoticeScope};
use orbit_types::tool::{McpCapability, ToolSessionContext};

use super::limits;
use crate::OrbitRuntime;
use crate::runtime::authorization::resolved_caller_capabilities;

/// What the tool layer knows about a notice caller.
#[derive(Debug, Clone, Default)]
pub struct NoticeCaller {
    /// The calling session; its capabilities and worker binding decide the
    /// class.
    pub session: ToolSessionContext,
    /// The task a run-stamped tool context names (`orbit tool run` inside a
    /// managed run).
    pub scope_task_id: Option<String>,
    /// The run a run-stamped tool context names.
    pub scope_run_id: Option<String>,
    /// Orchestrator attribution: the session's crew or a per-call name.
    pub orchestrator: Option<String>,
    /// The caller's model, when it named one.
    pub model: Option<String>,
}

/// A resolved poster.
#[derive(Debug, Clone)]
pub(crate) struct NoticePoster {
    pub(crate) class: AuthorClass,
    /// The crew an orchestrator posts as, or the bound task's crew.
    pub(crate) crew: Option<String>,
    pub(crate) model: Option<String>,
    /// The bound task of an agent.
    pub(crate) task_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) machine_id: Option<String>,
}

impl NoticePoster {
    pub(crate) fn author(&self) -> NoticeAuthor {
        NoticeAuthor {
            class: self.class,
            crew: self.crew.clone(),
            model: self.model.clone(),
            task_id: self.task_id.clone(),
            run_id: self.run_id.clone(),
            machine_id: self.machine_id.clone(),
        }
    }

    pub(crate) fn may_post_scope(&self, scope: NoticeScope) -> bool {
        match self.class {
            AuthorClass::Operator | AuthorClass::Orchestrator => true,
            AuthorClass::Agent | AuthorClass::System => scope == NoticeScope::Workspace,
        }
    }

    pub(crate) fn may_post_kind(&self, kind: NoticeKind) -> bool {
        match self.class {
            AuthorClass::Operator | AuthorClass::Orchestrator => true,
            AuthorClass::Agent | AuthorClass::System => kind != NoticeKind::Maintenance,
        }
    }

    /// Whether this poster may edit or close a notice by `author`.
    pub(crate) fn may_edit(&self, author_class: &str, author_task_id: Option<&str>) -> bool {
        match self.class {
            AuthorClass::Operator => true,
            // Its own crew's notices and any orchestrator- or agent-authored
            // one; never the operator's.
            AuthorClass::Orchestrator => matches!(author_class, "orchestrator" | "agent"),
            AuthorClass::Agent => {
                author_class == "agent"
                    && author_task_id.is_some()
                    && author_task_id == self.task_id.as_deref()
            }
            AuthorClass::System => false,
        }
    }

    /// The longest expiry this poster may set for `kind`, in hours.
    pub(crate) fn expiry_cap_hours(&self, kind: NoticeKind) -> u32 {
        match self.class {
            AuthorClass::Operator | AuthorClass::System => limits::MAX_EXPIRY_HOURS_OPERATOR,
            AuthorClass::Orchestrator => limits::max_expiry_hours_orchestrator(kind),
            AuthorClass::Agent => limits::MAX_EXPIRY_HOURS_AGENT,
        }
    }

    /// The expiry used when a post names none, in hours. An agent's default
    /// is its cap.
    pub(crate) fn default_expiry_hours(&self, kind: NoticeKind) -> u32 {
        match self.class {
            AuthorClass::Agent => limits::MAX_EXPIRY_HOURS_AGENT,
            AuthorClass::Operator | AuthorClass::Orchestrator | AuthorClass::System => {
                limits::default_expiry_hours(kind).min(self.expiry_cap_hours(kind))
            }
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

impl OrbitRuntime {
    /// Resolve the poster class of `caller`: operator capability first, then
    /// a managed-run binding, then orchestrator attribution naming a
    /// configured crew. Anything else is refused.
    pub(crate) fn resolve_notice_poster(
        &self,
        caller: &NoticeCaller,
    ) -> Result<NoticePoster, OrbitError> {
        let capabilities = resolved_caller_capabilities(&caller.session);
        let machine_id = self.automation_machine_identity().map(ToOwned::to_owned);
        let model = non_empty(caller.model.as_deref());
        if capabilities.contains(&McpCapability::Operator)
            && caller.session.worker_invocation.is_none()
        {
            return Ok(NoticePoster {
                class: AuthorClass::Operator,
                crew: None,
                model,
                task_id: None,
                run_id: None,
                machine_id,
            });
        }
        let bound = match &caller.session.worker_invocation {
            Some(binding) => {
                binding.validate().map_err(|error| {
                    OrbitError::notice_refused(
                        NoticeRefusalCode::PosterUnidentified,
                        format!("the worker binding is invalid: {error}"),
                    )
                })?;
                if binding.owner_workspace_id != self.workspace_id()? {
                    return Err(OrbitError::notice_refused(
                        NoticeRefusalCode::ScopeRefused,
                        "a managed agent posts only in its bound task's workspace",
                    ));
                }
                Some((
                    binding.task_id.clone(),
                    Some(binding.bound_run_id.clone()),
                    Some(binding.execution.machine_id.clone()),
                ))
            }
            None => match (
                non_empty(caller.scope_task_id.as_deref()),
                non_empty(caller.scope_run_id.as_deref()),
            ) {
                (Some(task_id), Some(run_id)) => Some((task_id, Some(run_id), machine_id.clone())),
                _ => None,
            },
        };
        if let Some((task_id, run_id, machine_id)) = bound {
            let task = self.get_task(&task_id).map_err(|_| {
                OrbitError::notice_refused(
                    NoticeRefusalCode::PosterUnidentified,
                    "the bound task is not in this workspace",
                )
            })?;
            return Ok(NoticePoster {
                class: AuthorClass::Agent,
                crew: task.crew.clone(),
                model,
                task_id: Some(task_id),
                run_id,
                machine_id,
            });
        }
        if capabilities.contains(&McpCapability::Agent)
            && let Some(orchestrator) = non_empty(caller.orchestrator.as_deref())
        {
            let crew = self
                .canonical_crew_name(Some(&orchestrator))
                .ok()
                .flatten()
                .ok_or_else(|| {
                    OrbitError::notice_refused(
                        NoticeRefusalCode::PosterUnidentified,
                        format!("orchestrator `{orchestrator}` is not a configured crew"),
                    )
                })?;
            return Ok(NoticePoster {
                class: AuthorClass::Orchestrator,
                crew: Some(crew),
                model,
                task_id: None,
                run_id: None,
                machine_id,
            });
        }
        Err(OrbitError::notice_refused(
            NoticeRefusalCode::PosterUnidentified,
            "only the operator, an orchestrator session or a managed agent run may write notices",
        ))
    }
}
