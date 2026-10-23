//! Shared field admission, writer classification and normalization.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::security::redaction::redact_all;
use orbit_types::identity::{all_agent_families, infer_agent_family_from_model};
use orbit_types::task::{
    LIGHT_DELIVERY_TAGS, Task, TaskHistoryEntry, TaskStatus, is_system_identity_tag,
    is_valid_orb_task_id, normalize_task_dependencies, normalize_task_tags, validate_os_tags,
    validate_task_dependencies_with,
};

use super::super::params::TaskUpdateParams;
use super::super::paths::{
    canonicalize_context_files_for_read, context_workspace_root, normalize_context_files_for_write,
};
use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CreationCrewAssignment, random_crew_ticket};

/// Admit only review decisions understood by the merge gate. Keep the caller's
/// case and alias, trimming only the surrounding whitespace; empty clears.
fn validate_pr_status(value: &str) -> Result<Option<String>, OrbitError> {
    let value = value.trim();
    match orbit_engine::normalize_review_decision(value).as_str() {
        "" => Ok(None),
        "APPROVED" | "CHANGES_REQUESTED" => Ok(Some(value.to_string())),
        _ => Err(OrbitError::InvalidInput(format!(
            "unknown PR status '{value}': expected `approve` or `request-changes` (empty string clears)"
        ))),
    }
}

pub(in super::super) struct ValidatedTaskFieldEdits {
    pub(in super::super) params: TaskUpdateParams,
    /// Set when this write assigned a crew explicitly or drew a replacement;
    /// the caller includes the draw source in the change history [ORB-12717].
    pub(in super::super) crew_assignment: Option<CreationCrewAssignment>,
    pub(in super::super) crew_source: Option<Option<String>>,
    pub(in super::super) crew_redraw_history: Option<TaskHistoryEntry>,
}

/// Who is editing an existing task's fields [ORB-16872].
///
/// Built from the same signals as `operator_write` in `update_task_locked`:
/// decision authority, canonical agent and model, the automation-actor label,
/// and whether this process declares an agent envelope. An attributed agent
/// or model is an agent writer even when that envelope is absent, which is
/// how a user-launched MCP server calls tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum TaskFieldWriter {
    /// May add `skip-review` and `skip-validation`.
    Operator,
    /// Attributed agent or tool writer, or any writer inside an agent envelope.
    Agent,
    /// In-process machinery that is neither an operator nor an agent.
    Machinery,
}

/// Classify `writer` for shared field admission.
///
/// A non-empty canonical agent, or a model that names or infers an agent
/// family, is an agent writer. A bare label such as `human` is not: trusted
/// in-process callers store that label in the model slot without attributing
/// an agent.
pub(in super::super) fn task_field_writer(
    operator_decision_authority: bool,
    canonical_agent: Option<&str>,
    canonical_model: Option<&str>,
    actor_label: &str,
) -> TaskFieldWriter {
    if agent_context_declared() || attributed_agent_identity(canonical_agent, canonical_model) {
        TaskFieldWriter::Agent
    } else if operator_decision_authority
        && !super::super::helpers::is_automation_actor(actor_label)
    {
        TaskFieldWriter::Operator
    } else {
        TaskFieldWriter::Machinery
    }
}

/// Whether this identity is an agent family or a model string that infers one.
fn attributed_agent_identity(agent: Option<&str>, model: Option<&str>) -> bool {
    let names_agent = |value: &str| {
        let value = value.trim();
        !value.is_empty()
            && (all_agent_families()
                .iter()
                .any(|family| value.eq_ignore_ascii_case(family))
                || infer_agent_family_from_model(value).is_some())
    };
    agent.is_some_and(names_agent) || model.is_some_and(names_agent)
}

impl OrbitRuntime {
    /// Re-check, under the task lock, the creation intent an operator
    /// surface's screening derived before the write: every newly authorized
    /// selector is part of the written scope, and a grant the screening
    /// relied on is still the task's grant. A concurrent write that revoked
    /// or replaced it makes this write fail instead of storing a missing
    /// target with no authorization behind it.
    fn ensure_context_creation_current(
        &self,
        task: &Task,
        creation: &super::super::ContextCreationAuthorization,
        context_files: &[String],
    ) -> Result<(), OrbitError> {
        if let Some(outside) = creation
            .authorize
            .iter()
            .find(|selector| !context_files.contains(selector))
        {
            return Err(OrbitError::InvalidInput(format!(
                "creation authorization for `{outside}` does not match a selector this write stores"
            )));
        }
        if let Some(relied_on) = &creation.relied_on
            && self.context_creation_state(task)?.identity().as_ref() != Some(relied_on)
        {
            return Err(OrbitError::InvalidInput(format!(
                "task '{}' context creation authorization changed during this write; \
                 re-read the task and retry",
                task.id
            )));
        }
        Ok(())
    }

    /// Apply the validation and canonicalization shared by ordinary updates
    /// and the guarded start body before either path reaches the record layer.
    ///
    /// `writer` is this caller's trusted class. Light-delivery tag additions
    /// consult it, not the process environment alone [ORB-16872].
    pub(in super::super) fn validate_and_normalize_task_field_edits(
        &self,
        id: &str,
        task: &Task,
        mut params: TaskUpdateParams,
        writer: TaskFieldWriter,
    ) -> Result<ValidatedTaskFieldEdits, OrbitError> {
        // Both ordinary updates and guarded starts reach this application
        // boundary, including CLI/dashboard callers that bypass tool-host
        // sanitization. Scrub prose before it enters a bundle or write journal.
        for value in [
            &mut params.title,
            &mut params.description,
            &mut params.plan,
            &mut params.execution_summary,
            &mut params.comment,
        ]
        .into_iter()
        .flatten()
        {
            *value = redact_all(value);
        }
        if let Some(criteria) = &mut params.acceptance_criteria {
            for criterion in criteria {
                *criterion = redact_all(criterion);
            }
        }
        if let Some(Some(value)) = &params.pr_status {
            params.pr_status = Some(validate_pr_status(value)?);
        }
        let context_root = context_workspace_root(&self.paths().repo_root, None);
        if let Some(candidates) = params.context_files.take() {
            let candidates = normalize_context_files_for_write(candidates, &context_root)?;
            self.ensure_context_creation_current(task, &params.context_creation, &candidates)?;
            params.context_files = Some(candidates);
        } else {
            // Creation intent describes a replacement scope; without one there
            // is nothing it could authorize.
            params.context_creation = Default::default();
            // An unrelated edit re-canonicalizes what is already stored and
            // writes the result back only when every declaration survived and
            // the canonical form differs. It never drops one: a selector whose
            // target does not exist is scope the task still owns, and losing
            // it here would silently shrink the footprint its locks protect.
            // A selector that cannot be canonicalized at all is left stored
            // verbatim for `orbit task lint` to report [ORB-12490].
            let canonical = canonicalize_context_files_for_read(&task.context_files, &context_root);
            if canonical.len() == task.context_files.len() && canonical != task.context_files {
                params.context_files = Some(canonical);
            }
        }
        if let Some(dependencies) = params.dependencies.take() {
            let normalized_dependencies = normalize_task_dependencies(dependencies)?;
            validate_task_dependencies_with(Some(id), &normalized_dependencies, |dep_id| {
                if !is_valid_orb_task_id(dep_id) {
                    return Ok::<_, OrbitError>(None);
                }
                Ok(self
                    .stores()
                    .tasks()
                    .get_task(dep_id)?
                    .map(|task| task.dependencies()))
            })?;
            params.dependencies = Some(normalized_dependencies);
        }
        if let Some(tags) = params.tags.take() {
            let tags = normalize_task_tags(tags);
            validate_os_tags(&tags)?;
            if !params.allow_drop_system_tags {
                for existing in &task.tags {
                    if is_system_identity_tag(existing) {
                        let normalized_existing = existing.trim().to_ascii_lowercase();
                        if !tags.iter().any(|t| t == &normalized_existing) {
                            return Err(OrbitError::SystemIdentityTagDropped {
                                tag: existing.clone(),
                            });
                        }
                    }
                }
            }
            // [ORB-16855] [ORB-16872] A light-delivery tag skips review or
            // validation, so only filing or an operator may add it. An
            // attributed agent or tool writer is refused even when this
            // process has no agent envelope — the normal shape of a
            // user-launched MCP server. Removing a tag stays allowed.
            if writer == TaskFieldWriter::Agent
                && let Some(added) = LIGHT_DELIVERY_TAGS.iter().find(|tag| {
                    tags.iter().any(|new| new == *tag) && !task.tags.iter().any(|old| old == *tag)
                })
            {
                return Err(OrbitError::CapabilityDenied(format!(
                    "an agent cannot add `{added}` to an existing task; only filing the task \
                     or an operator update sets it"
                )));
            }
            params.tags = Some(tags);
        }
        // [ORB-12717] Clearing the crew is "no crew supplied", so the pools
        // decide again for the complexity this write leaves the task with —
        // a re-queue after a provider failure lands on a fresh draw instead of
        // on nothing. A pool assignment from another tier or a default fallback
        // is redrawn when complexity changes too.
        let mut crew_assignment = None;
        let mut crew_redraw_history = None;
        if let Some(crew) = &mut params.crew {
            *crew = self.canonical_crew_name(crew.as_deref())?;
            if crew.is_none() {
                crew_assignment = self.creation_crew_assignment(
                    params.complexity.or(task.complexity),
                    None,
                    params.tags.as_deref().unwrap_or(&task.tags),
                    &mut random_crew_ticket,
                )?;
                *crew = crew_assignment
                    .as_ref()
                    .map(|assignment| assignment.crew.clone());
            } else {
                // An explicit update must not pin a disabled crew onto a task.
                self.resolve_crew_for_task(None, crew.as_deref())?;
                crew_assignment = crew.as_ref().map(|crew| CreationCrewAssignment {
                    crew: crew.clone(),
                    source: "explicit".to_string(),
                });
            }
        }
        let mut crew_source = params.crew.as_ref().map(|_| {
            crew_assignment
                .as_ref()
                .map(|assignment| assignment.source.clone())
        });
        if params.crew.is_none()
            && let Some(complexity) = params.complexity
            && Some(complexity) != task.complexity
        {
            let mut rerated = task.clone();
            if let Some(tags) = &params.tags {
                rerated.tags.clone_from(tags);
            }
            crew_redraw_history = self.rerate_task_crew(&mut rerated, Some(complexity))?;
            if crew_redraw_history.is_some() {
                params.crew = Some(rerated.crew);
                crew_source = Some(rerated.crew_source);
            }
        }
        if let Some(orchestrator) = &mut params.orchestrator {
            *orchestrator = self.canonical_crew_name(orchestrator.as_deref())?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Err(OrbitError::InvalidInput(format!(
                    "task {id} is {}; orchestrator can only be changed while proposed or backlog",
                    task.status
                )));
            }
        }
        if params.crew.is_some() || params.tags.is_some() {
            let tags = params.tags.as_deref().unwrap_or(&task.tags);
            let crew = match &params.crew {
                Some(crew) => crew.clone(),
                None => task.crew.clone(),
            };
            if let Some(crew) = crew.as_deref() {
                self.refuse_unsatisfiable_os_crew(tags, crew)?;
            }
        }
        if params.status == Some(TaskStatus::Done) && task.status != TaskStatus::Done {
            let mut preview = task.clone();
            if let Some(relations) = &params.relations {
                preview.relations = relations.clone();
            }
            self.ensure_resolves_are_workspace_local(&preview)?;
        }

        Ok(ValidatedTaskFieldEdits {
            params,
            crew_assignment,
            crew_source,
            crew_redraw_history,
        })
    }
}
