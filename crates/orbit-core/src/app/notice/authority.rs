//! Who is posting, and what that poster may do.
//!
//! The poster class comes from the caller's resolved capabilities and its run
//! binding, never from input. Orchestrator attribution is self-declared (the
//! same trust task filing extends), so the rights it unlocks are the ones a
//! mistaken unmanaged session could use without lasting harm.

use crate::app::resident;
use chrono::Duration;
use orbit_common::{NoticeRefusalCode, OrbitError};
use orbit_tools::ActivityBinding;
use orbit_types::record::{
    AuthorClass, NoticeAuthor, NoticeKind, NoticeScope, resident_notice_key_prefix,
    resident_status_notice_key,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRun, RESIDENT_CYCLE_JOB, RESIDENT_TAG};

use super::limit as limits;
use crate::OrbitRuntime;
use crate::runtime::authorization::resolved_caller_capabilities;
use crate::runtime::run_input::carried_task_ids;

/// What the tool layer knows about a notice caller.
#[derive(Debug, Clone, Default)]
pub struct NoticeCaller {
    /// The calling session; its capabilities and worker binding decide the
    /// class.
    pub session: ToolSessionContext,
    /// The task a run-stamped tool context names (`orbit tool run` inside a
    /// managed run).
    pub scope_task_id: Option<String>,
    /// The managed activity the dispatching host attested for this call:
    /// the only source of a non-resident managed run's leaf task. The tool
    /// host sets it from its own run binding; tool input and the process
    /// environment the notice flow could re-read never do.
    pub activity: Option<ActivityBinding>,
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
    /// Set when the poster is a resident mandate: its cycle's run, or Orbit
    /// writing the mandate's line for a control action. `task_id` is then
    /// the mandate's assignment.
    pub(crate) resident: Option<ResidentNoticePoster>,
}

/// What a resident mandate's notice posts are held to.
#[derive(Debug, Clone)]
pub(crate) struct ResidentNoticePoster {
    /// How long the status line stays in force after the post that wrote it.
    pub(crate) status_line_ttl: Duration,
}

impl NoticePoster {
    /// The key of this resident's status line, when the poster is one.
    pub(crate) fn resident_status_key(&self) -> Option<String> {
        self.resident.as_ref()?;
        self.task_id.as_deref().map(resident_status_notice_key)
    }

    /// Whether this poster may write `key`: keys under `resident:` belong to
    /// the mandate they name.
    pub(crate) fn may_post_key(&self, key: &str) -> bool {
        if !key.starts_with(orbit_types::record::RESIDENT_NOTICE_KEY_PREFIX) {
            return true;
        }
        self.resident.is_some()
            && self
                .task_id
                .as_deref()
                .is_some_and(|mandate| key.starts_with(&resident_notice_key_prefix(mandate)))
    }

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
            // An operator ask is the operator's, an orchestrator's or a
            // resident's to raise; other managed agents report through
            // their task.
            AuthorClass::Agent | AuthorClass::System => match kind {
                NoticeKind::Incident | NoticeKind::Info => true,
                NoticeKind::Warn => self.resident.is_some(),
                NoticeKind::Maintenance => false,
            },
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
            // A resident's warn stands until its blocker clears, which a
            // day rarely covers.
            AuthorClass::Agent if self.resident.is_some() && kind == NoticeKind::Warn => {
                limits::max_expiry_hours_orchestrator(kind)
            }
            AuthorClass::Agent => limits::MAX_EXPIRY_HOURS_AGENT,
        }
    }

    /// The expiry used when a post names none, in hours. An agent's default
    /// is its cap.
    pub(crate) fn default_expiry_hours(&self, kind: NoticeKind) -> u32 {
        match self.class {
            AuthorClass::Agent if self.resident.is_some() && kind == NoticeKind::Warn => {
                limits::default_expiry_hours(kind)
            }
            AuthorClass::Agent => limits::MAX_EXPIRY_HOURS_AGENT,
            AuthorClass::Operator | AuthorClass::Orchestrator | AuthorClass::System => {
                limits::default_expiry_hours(kind).min(self.expiry_cap_hours(kind))
            }
        }
    }
}

/// The notice terms of the mandate on `task_id`: its status line lapses
/// after twice the heartbeat, the same silence the stall check uses. A
/// mandate that cannot be read falls back to the agent expiry.
fn resident_notice_poster(rt: &OrbitRuntime, task_id: &str) -> ResidentNoticePoster {
    let status_line_ttl = match crate::app::resident::resident_mandate_from_artifact(rt, task_id) {
        Ok(Ok(mandate)) => Duration::minutes(2 * i64::from(mandate.cadence.heartbeat_minutes)),
        Ok(Err(_)) | Err(_) => Duration::hours(i64::from(limits::MAX_EXPIRY_HOURS_AGENT)),
    };
    ResidentNoticePoster { status_line_ttl }
}

/// The poster Orbit writes a mandate's control line as: the mandate's
/// own assignment, so the next cycle's status line replaces it in place.
///
/// A line for a mandate that stays quiet (`lasting`: paused, taken over,
/// stopped) outlives the heartbeat; one for a mandate that should cycle
/// again lapses like a cycle's own line, so a mandate that never does
/// reads as silent.
pub(super) fn resident_control_line_poster(
    rt: &OrbitRuntime,
    task_id: &str,
    lasting: bool,
) -> Result<NoticePoster, OrbitError> {
    let task = rt.get_task(task_id)?;
    let resident = if lasting {
        ResidentNoticePoster {
            status_line_ttl: Duration::hours(i64::from(limits::RESIDENT_CONTROL_LINE_EXPIRY_HOURS)),
        }
    } else {
        resident_notice_poster(rt, task_id)
    };
    Ok(NoticePoster {
        class: AuthorClass::Agent,
        crew: task.crew.clone(),
        model: None,
        task_id: Some(task.id.to_string()),
        run_id: None,
        machine_id: rt.automation_machine_identity().map(ToOwned::to_owned),
        resident: Some(resident),
    })
}

/// Resolve the poster class of `caller`: operator capability first, then
/// a managed-run binding, then orchestrator attribution naming a
/// configured crew. Anything else is refused.
pub(super) fn resolve_notice_poster(
    rt: &OrbitRuntime,
    caller: &NoticeCaller,
) -> Result<NoticePoster, OrbitError> {
    let capabilities = resolved_caller_capabilities(rt.env(), &caller.session);
    let machine_id = rt.automation_machine_identity().map(ToOwned::to_owned);
    let model = non_empty(caller.model.as_deref());
    let scope_task_id = non_empty(caller.scope_task_id.as_deref());
    let scope_run_id = non_empty(caller.scope_run_id.as_deref());
    let has_managed_binding = caller.session.worker_invocation.is_some()
        || scope_run_id.is_some()
        || scope_task_id.is_some();
    if capabilities.contains(&McpCapability::Operator) && !has_managed_binding {
        return Ok(NoticePoster {
            class: AuthorClass::Operator,
            crew: None,
            model,
            task_id: None,
            run_id: None,
            machine_id,
            resident: None,
        });
    }
    let resolve_managed_binding = |run_id: &str, scope_task_id: Option<&str>| {
        let run = rt
            .stores()
            .jobs()
            .get_job_run(run_id)
            .map_err(|_| {
                OrbitError::notice_refused(
                    NoticeRefusalCode::PosterUnidentified,
                    "only the operator, an orchestrator session or a managed agent run may write notices",
                )
            })?
            .ok_or_else(|| {
                OrbitError::notice_refused(
                    NoticeRefusalCode::PosterUnidentified,
                    format!("run `{run_id}` is not a managed run in this workspace"),
                )
            })?;
        let resident_mandate = if run.job_id == RESIDENT_CYCLE_JOB {
            Some(
                resident::resident_cycle_mandate_of_run(rt, run_id).map_err(|_| {
                    OrbitError::notice_refused(
                        NoticeRefusalCode::PosterUnidentified,
                        format!("resident run `{run_id}` has no single bound mandate"),
                    )
                })?,
            )
        } else {
            None
        };
        let leaf_task = match resident_mandate {
            Some(_) => scope_task_id.map(ToOwned::to_owned),
            None => Some(attested_activity_task(
                caller.activity.as_ref(),
                run_id,
                scope_task_id,
            )?),
        };
        resolve_managed_run_task_binding(
            &run,
            run_id,
            leaf_task.as_deref(),
            resident_mandate.as_deref(),
        )
    };
    let bound = match &caller.session.worker_invocation {
        Some(binding) => {
            binding.validate().map_err(|error| {
                OrbitError::notice_refused(
                    NoticeRefusalCode::PosterUnidentified,
                    format!("the worker binding is invalid: {error}"),
                )
            })?;
            if binding.owner_workspace_id != rt.workspace_id()? {
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
        None => match (scope_task_id, scope_run_id) {
            (Some(task_id), Some(run_id)) => {
                let bound_task = resolve_managed_binding(&run_id, Some(&task_id))?;
                Some((bound_task, Some(run_id), machine_id.clone()))
            }
            // A resident cycle is bound by its run alone: the run names
            // its mandate.
            (None, Some(run_id)) => {
                let mandate = resolve_managed_binding(&run_id, None)?;
                Some((mandate, Some(run_id), machine_id.clone()))
            }
            (Some(_), None) => {
                return Err(OrbitError::notice_refused(
                    NoticeRefusalCode::PosterUnidentified,
                    "a managed agent run requires both a run and task binding",
                ));
            }
            (None, None) => None,
        },
    };
    if let Some((task_id, run_id, machine_id)) = bound {
        let task = rt.get_task(&task_id).map_err(|_| {
            OrbitError::notice_refused(
                NoticeRefusalCode::PosterUnidentified,
                "the bound task is not in this workspace",
            )
        })?;
        let resident = run_id
            .as_deref()
            .filter(|_| task.tags.iter().any(|tag| tag == RESIDENT_TAG))
            .filter(|run_id| {
                resident::resident_cycle_mandate_of_run(rt, run_id)
                    .is_ok_and(|mandate| mandate == task_id)
            })
            .map(|_| resident_notice_poster(rt, &task_id));
        return Ok(NoticePoster {
            class: AuthorClass::Agent,
            crew: task.crew.clone(),
            model,
            task_id: Some(task_id),
            run_id,
            machine_id,
            resident,
        });
    }
    if capabilities.contains(&McpCapability::Agent)
        && let Some(orchestrator) = non_empty(caller.orchestrator.as_deref())
    {
        let crew = rt
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
            resident: None,
        });
    }
    Err(OrbitError::notice_refused(
        NoticeRefusalCode::PosterUnidentified,
        "only the operator, an orchestrator session or a managed agent run may write notices",
    ))
}

/// The leaf task the dispatching host attested for `run_id`'s activity.
///
/// A non-resident managed run is bound only by that attestation: one that is
/// missing, names another run, names no single task (an ambiguous bundle) or
/// disagrees with the call's task scope refuses.
fn attested_activity_task(
    activity: Option<&ActivityBinding>,
    run_id: &str,
    scope_task_id: Option<&str>,
) -> Result<String, OrbitError> {
    let unidentified = |detail: String| {
        Err(OrbitError::notice_refused(
            NoticeRefusalCode::PosterUnidentified,
            detail,
        ))
    };
    let Some(activity) = activity.filter(|binding| binding.job_run_id.trim() == run_id) else {
        return unidentified(format!(
            "managed run `{run_id}` has no host-attested activity binding"
        ));
    };
    let Some(task_id) = non_empty(activity.task_id.as_deref()) else {
        return unidentified(format!(
            "managed run `{run_id}` has no single attested activity task"
        ));
    };
    if scope_task_id != Some(task_id.as_str()) {
        return unidentified(format!(
            "managed run `{run_id}` has no matching active activity task"
        ));
    }
    Ok(task_id)
}

/// Authenticate that `run` is a managed run for `scope_task_id`, or a resident
/// cycle run whose single bound mandate is returned.
fn resolve_managed_run_task_binding(
    run: &JobRun,
    run_id: &str,
    scope_task_id: Option<&str>,
    resident_mandate: Option<&str>,
) -> Result<String, OrbitError> {
    if run.job_id == RESIDENT_CYCLE_JOB {
        let mandate = resident_mandate.ok_or_else(|| {
            OrbitError::notice_refused(
                NoticeRefusalCode::PosterUnidentified,
                format!("resident run `{run_id}` has no single bound mandate"),
            )
        })?;
        if let Some(task_id) = scope_task_id
            && task_id != mandate
        {
            return Err(OrbitError::notice_refused(
                NoticeRefusalCode::PosterUnidentified,
                format!("resident run `{run_id}` is bound to mandate `{mandate}`, not `{task_id}`"),
            ));
        }
        return Ok(mandate.to_string());
    }
    let Some(task_id) = scope_task_id else {
        return Err(OrbitError::notice_refused(
            NoticeRefusalCode::PosterUnidentified,
            format!("managed run `{run_id}` has no resolved activity task"),
        ));
    };
    let carries_task = run
        .input
        .as_ref()
        .is_some_and(|input| carried_task_ids(input).contains(task_id));
    if !carries_task {
        return Err(OrbitError::notice_refused(
            NoticeRefusalCode::PosterUnidentified,
            format!("run `{run_id}` is not a managed run of task `{task_id}`"),
        ));
    }
    Ok(task_id.to_string())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
