use super::*;

#[derive(Default)]
struct StartTaskOptions {
    note: Option<String>,
    comment: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    actor_label_override: Option<String>,
    crew_override: Option<String>,
    plan: Option<String>,
    field_edits: TaskUpdateParams,
    artifact_owner: Option<String>,
}

fn start_body_field_edits(
    mut field_edits: TaskUpdateParams,
    plan: Option<String>,
) -> TaskUpdateParams {
    field_edits.status = Some(TaskStatus::InProgress);
    if plan.is_some() {
        field_edits.plan = plan;
    }
    field_edits.comment = None;
    field_edits
}

/// Append proposed-start approval context to a post-approval start failure
/// without changing its variant, so MCP/CLI codes and `is_lock_busy` /
/// `claim_refusal` classification answer from the variant alone.
fn proposed_start_error_with_context(error: OrbitError, state: &str) -> OrbitError {
    let suffix = format!("; {state}");
    match error {
        OrbitError::InvalidInput(message) => OrbitError::InvalidInput(format!("{message}{suffix}")),
        OrbitError::Execution(message) => OrbitError::Execution(format!("{message}{suffix}")),
        OrbitError::Store(message) => OrbitError::Store(format!("{message}{suffix}")),
        OrbitError::Io(message) => OrbitError::Io(format!("{message}{suffix}")),
        OrbitError::WorkspaceError(message) => {
            OrbitError::WorkspaceError(format!("{message}{suffix}"))
        }
        OrbitError::Migration(message) => OrbitError::Migration(format!("{message}{suffix}")),
        OrbitError::PolicyDenied(message) => OrbitError::PolicyDenied(format!("{message}{suffix}")),
        OrbitError::CapabilityDenied(message) => {
            OrbitError::CapabilityDenied(format!("{message}{suffix}"))
        }
        OrbitError::UnknownSelector(message) => {
            OrbitError::UnknownSelector(format!("{message}{suffix}"))
        }
        OrbitError::AmbiguousDestination(message) => {
            OrbitError::AmbiguousDestination(format!("{message}{suffix}"))
        }
        OrbitError::UnreachableDestination(message) => {
            OrbitError::UnreachableDestination(format!("{message}{suffix}"))
        }
        OrbitError::StaleRoute(message) => OrbitError::StaleRoute(format!("{message}{suffix}")),
        OrbitError::UnhealthyCheckout(message) => {
            OrbitError::UnhealthyCheckout(format!("{message}{suffix}"))
        }
        OrbitError::ToolNotOnThisHost(message) => {
            OrbitError::ToolNotOnThisHost(format!("{message}{suffix}"))
        }
        OrbitError::CapabilityRefused(message) => {
            OrbitError::CapabilityRefused(format!("{message}{suffix}"))
        }
        OrbitError::ProtocolSkew(message) => OrbitError::ProtocolSkew(format!("{message}{suffix}")),
        OrbitError::PluginBuildConsentRequired(message) => {
            OrbitError::PluginBuildConsentRequired(format!("{message}{suffix}"))
        }
        OrbitError::PluginBuildConsentUnavailable(message) => {
            OrbitError::PluginBuildConsentUnavailable(format!("{message}{suffix}"))
        }
        OrbitError::PluginBuildFetchUnsupported(message) => {
            OrbitError::PluginBuildFetchUnsupported(format!("{message}{suffix}"))
        }
        OrbitError::SkillValidation(message) => {
            OrbitError::SkillValidation(format!("{message}{suffix}"))
        }
        OrbitError::JobValidation(message) => {
            OrbitError::JobValidation(format!("{message}{suffix}"))
        }
        OrbitError::AgentProtocolViolation(message) => {
            OrbitError::AgentProtocolViolation(format!("{message}{suffix}"))
        }
        OrbitError::UnsupportedAgentProvider(message) => {
            OrbitError::UnsupportedAgentProvider(format!("{message}{suffix}"))
        }
        OrbitError::OwnerUnavailable(message) => {
            OrbitError::OwnerUnavailable(format!("{message}{suffix}"))
        }
        OrbitError::OwnerNegotiation(message) => {
            OrbitError::OwnerNegotiation(format!("{message}{suffix}"))
        }
        OrbitError::JobRunStateTransition(message) => {
            OrbitError::JobRunStateTransition(format!("{message}{suffix}"))
        }
        OrbitError::JobRunStartConflict(message) => {
            OrbitError::JobRunStartConflict(format!("{message}{suffix}"))
        }
        OrbitError::JobRunControlConflict(message) => {
            OrbitError::JobRunControlConflict(format!("{message}{suffix}"))
        }
        OrbitError::ClaimRefused { kind, message } => OrbitError::ClaimRefused {
            kind,
            message: format!("{message}{suffix}"),
        },
        OrbitError::NotFound { kind, id, guidance } => OrbitError::NotFound {
            kind,
            id,
            guidance: Some(match guidance {
                Some(existing) => format!("{existing}{suffix}"),
                None => suffix.clone(),
            }),
        },
        OrbitError::InvalidInputDiagnostic {
            message,
            did_you_mean,
        } => OrbitError::InvalidInputDiagnostic {
            message: format!("{message}{suffix}"),
            did_you_mean,
        },
        OrbitError::HostRegistry { code, message } => OrbitError::HostRegistry {
            code,
            message: format!("{message}{suffix}"),
        },
        OrbitError::DecisionRefused { code, message } => OrbitError::DecisionRefused {
            code,
            message: format!("{message}{suffix}"),
        },
        OrbitError::SensitiveInput { field, reason } => OrbitError::SensitiveInput {
            field,
            reason: format!("{reason}{suffix}"),
        },
        OrbitError::OutcomeUnknown {
            mcp_call_id,
            message,
        } => OrbitError::OutcomeUnknown {
            mcp_call_id,
            message: format!("{message}{suffix}"),
        },
        OrbitError::RemoteTool {
            code,
            message,
            payload,
        } => OrbitError::RemoteTool {
            code,
            message: format!("{message}{suffix}"),
            payload,
        },
        OrbitError::DeliveryRefused { refusal, message } => OrbitError::DeliveryRefused {
            refusal,
            message: format!("{message}{suffix}"),
        },
        OrbitError::ExecutionTimeout {
            timeout_ms,
            message,
        } => OrbitError::ExecutionTimeout {
            timeout_ms,
            message: format!("{message}{suffix}"),
        },
        OrbitError::ProcessTimeout { timeout_ms, detail } => OrbitError::ProcessTimeout {
            timeout_ms,
            detail: format!("{detail}{suffix}"),
        },
        OrbitError::WorkerContainmentUnavailable { reason } => {
            OrbitError::WorkerContainmentUnavailable {
                reason: format!("{reason}{suffix}"),
            }
        }
        OrbitError::RunProviderTeardownUnconfirmed { pid, reason } => {
            OrbitError::RunProviderTeardownUnconfirmed {
                pid,
                reason: format!("{reason}{suffix}"),
            }
        }
        OrbitError::TaskBundleCorrupt {
            task_id,
            path,
            reason,
        } => OrbitError::TaskBundleCorrupt {
            task_id,
            path,
            reason: format!("{reason}{suffix}"),
        },
        OrbitError::DesktopWriteAccepted { task_id, reason } => OrbitError::DesktopWriteAccepted {
            task_id,
            reason: format!("{reason}{suffix}"),
        },
        OrbitError::SqliteContention(mut contention) => {
            contention.detail = format!("{}{suffix}", contention.detail);
            OrbitError::SqliteContention(contention)
        }
        OrbitError::StorageAccessDenied { layer, message } => OrbitError::StorageAccessDenied {
            layer,
            message: format!("{message}{suffix}"),
        },
        OrbitError::StorageLockBusy { layer, message } => OrbitError::StorageLockBusy {
            layer,
            message: format!("{message}{suffix}"),
        },
        // Variants without a free-text slot keep their payload unchanged so
        // the classification still answers from the variant alone.
        other => other,
    }
}

impl OrbitRuntime {
    pub fn start_task(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                ..Default::default()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_task_with_identity_and_crew(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        crew_override: Option<String>,
        plan: Option<String>,
        field_edits: TaskUpdateParams,
        artifact_owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                agent,
                model,
                crew_override,
                plan,
                field_edits,
                artifact_owner,
                ..Default::default()
            },
        )
    }

    pub(crate) fn start_task_as_system(
        &self,
        id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.start_task_with_actor_label_override(
            id,
            StartTaskOptions {
                note,
                comment,
                actor_label_override: Some(SYSTEM_ACTOR_LABEL.to_string()),
                ..Default::default()
            },
        )
    }

    fn start_task_with_actor_label_override(
        &self,
        id: &str,
        options: StartTaskOptions,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let StartTaskOptions {
            note,
            comment,
            agent,
            model,
            actor_label_override,
            crew_override,
            plan,
            field_edits,
            artifact_owner,
        } = options;
        let note = note.map(|value| redact_all(&value));
        let comment = comment.map(|value| redact_all(&value));
        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = self.actor().clone();
        let effective_label = match actor_label_override.clone() {
            Some(label) => label,
            None => effective_actor_label(
                &actor.label,
                canonical_agent.as_deref(),
                canonical_model.as_deref(),
            )?,
        };
        let append_comments = build_task_comments(comment, effective_label.as_str())?;
        let mut started = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if actor_label_override.is_none() && task.status != TaskStatus::InProgress {
                ensure_status_change_not_run_owned(self, id)?;
            }
            #[cfg(test)]
            self.apply_transition_read_hook(id)?;
            // Validate status before crew resolution so a misleading
            // "no crew selected" error can't mask the real problem
            // (e.g. trying to restart a task that's already in-progress).
            match task.status {
                TaskStatus::Proposed
                | TaskStatus::Backlog
                | TaskStatus::Someday
                | TaskStatus::Blocked => {}
                TaskStatus::InProgress => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is already in-progress"
                    )));
                }
                other => {
                    return Err(OrbitError::InvalidInput(format!(
                        "task '{id}' is in status '{other}'; start requires 'proposed', 'backlog', 'someday', or 'blocked'"
                    )));
                }
            }
            let mut start_edits = start_body_field_edits(field_edits.clone(), plan.clone());
            // A start accepts the same caller-supplied artifacts as an ordinary
            // update. Admit the whole batch before approval or record writes.
            for artifact in &mut start_edits.upsert_artifacts {
                admit_task_artifact(self, &task, artifact)?;
            }
            // A start has no operator-decision grant. An attributed agent or
            // model, or an agent envelope, still cannot add a light-delivery
            // tag on the way into `in-progress` [ORB-16872].
            let writer = super::super::update::task_field_writer(
                false,
                canonical_agent.as_deref(),
                canonical_model.as_deref(),
                effective_label.as_str(),
            );
            let validated =
                self.validate_and_normalize_task_field_edits(id, &task, start_edits, writer)?;
            let crew_assignment = validated.crew_assignment;
            let crew_source = validated.crew_source;
            let crew_redraw_history = validated.crew_redraw_history;
            let start_edits = validated.params;
            // Starting a proposed or someday task promotes it, so it is judged
            // like an ordinary promotion: on the finding material the start's
            // normalized edits leave, with the stored status kept, before
            // approval, history or field writes.
            if matches!(task.status, TaskStatus::Proposed | TaskStatus::Someday) {
                self.ensure_promoted_finding_verified(&task, &start_edits)?;
            }
            let resolved_crew_override = if start_edits.crew.is_some() {
                start_edits.crew.clone().flatten()
            } else {
                self.canonical_task_crew_name(crew_override.as_deref())?
            };
            // An operator starting a human crew's work has no agent crew to
            // resolve; the dispatch refusal is for agents only.
            let starting_crew = resolved_crew_override.as_deref().or(task.crew.as_deref());
            if !self.is_human_crew(starting_crew) {
                self.resolve_and_log_crew_for_task_start(
                    id,
                    resolved_crew_override.as_deref(),
                    task.crew.as_deref(),
                )?;
            }
            let dependency_status_index = self.dependency_status_index([&task])?;
            let unmet_dependencies = unmet_task_dependencies(&task, &dependency_status_index);
            let effective_plan = start_edits.plan.as_deref().unwrap_or(task.plan.as_str());
            if in_progress_transition_requires_plan(task.status) {
                ensure_task_has_execution_plan(id, effective_plan)?;
            }
            let unmet_dependency_labels: Vec<String> = unmet_dependencies
                .iter()
                .map(|dependency| dependency.label())
                .collect();
            let warn_unmet_dependencies = || {
                if !unmet_dependency_labels.is_empty() {
                    orbit_common::tracing::warn!(
                        target: "orbit.task.dependencies",
                        task_id = id,
                        unmet = unmet_dependency_labels.join(",").as_str(),
                        "task has unmet dependencies",
                    );
                }
            };
            // Attribute a plan written on this start to the agent family that
            // wrote it, the way the worker path does, unless the task already
            // names a planner or the caller sets one explicitly.
            let attribution = assemble_task_attribution(
                &task,
                TaskAttributionInput {
                    default_actor_label: &actor.label,
                    actor_override: actor_label_override.as_deref(),
                    agent: canonical_agent.as_deref(),
                    model: canonical_model.as_deref(),
                    runtime_model_identity: None,
                    plan_changed: start_edits.plan.is_some() && task.planned_by.is_none(),
                    // A start is never implementation evidence, and
                    // `start_edits` already carries any explicit
                    // `implemented_by` straight through to the store.
                    target_status: None,
                    explicit_planned_by: start_edits.planned_by.as_ref(),
                    explicit_implemented_by: None,
                },
            )?;
            let approved_from_proposed = task.status == TaskStatus::Proposed;
            warn_unmet_dependencies();
            started = Some(self.with_mutation(|| {
                // A start from `proposed` is two transitions, and history must
                // read that way: approve into backlog first, then start from
                // backlog. Writing them as two record updates under the task
                // lock lets the store derive each event's `from_status` from
                // the status it actually left, instead of stamping `started`
                // with `proposed` after a hand-built approval entry.
                //
                // The start write can still be refused after approval, for
                // example by a concurrent claim. Report that partial outcome.
                if approved_from_proposed {
                    self.stores().task_records().update(
                        id,
                        StoreTaskUpdateParams {
                            actor: effective_label.clone(),
                            status_event: Some("proposal_approved".to_string()),
                            status_note: note.clone(),
                            expected_status: Some(vec![TaskStatus::Proposed]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::Backlog),
                                ..Default::default()
                            })
                        },
                    )?;
                }
                let started = self.stores().task_records().update(
                    id,
                    StoreTaskUpdateParams {
                        actor: effective_label.clone(),
                        planned_by: attribution.planned_by.clone(),
                        status_event: Some("started".to_string()),
                        status_note: (!approved_from_proposed)
                            .then(|| note.clone())
                            .flatten(),
                        append_comments: append_comments.clone(),
                        crew_source: crew_source.clone(),
                        append_history: crew_assignment
                            .as_ref()
                            .map(crew_assigned_history)
                            .into_iter()
                            .chain(crew_redraw_history.clone())
                            .collect(),
                        artifact_owner_run_id: artifact_owner.clone(),
                        expected_status: Some(vec![if approved_from_proposed {
                            TaskStatus::Backlog
                        } else {
                            task.status
                        }]),
                        ..StoreTaskUpdateParams::from(start_edits.clone())
                    },
                )?;
                Ok((
                    started,
                    OrbitEvent::TaskStarted {
                        id: id.to_string(),
                        started_by: effective_label.clone(),
                        approved_from_proposed,
                    },
                ))
            }).map_err(|error| {
                if !approved_from_proposed {
                    return error;
                }
                let state = match self.get_task(id) {
                    Ok(current) if current.status == TaskStatus::Proposed => return error,
                    Ok(current) => format!(
                        "approval was recorded; fresh task status is '{}'. A backlog task may be selected by a drain; if work must not start, promptly move it to blocked, then resolve the refusal before retrying",
                        current.status
                    ),
                    Err(read_error) => format!(
                        "approval may have been recorded, but the fresh task status could not be read ({read_error}); inspect the task before retrying because backlog work may be selected by a drain"
                    ),
                };
                proposed_start_error_with_context(error, &state)
            })?);
            Ok(())
        })?;
        started.ok_or_else(|| {
            OrbitError::Execution("task start body did not run under the task lock".to_string())
        })
    }
}
