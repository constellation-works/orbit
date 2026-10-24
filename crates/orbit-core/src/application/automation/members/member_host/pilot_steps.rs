use super::*;

/// One deterministic step of the definition a run executed: its id and its
/// global step index, the key of its checkpoint.
#[derive(Clone, Copy)]
pub(super) struct PilotStep {
    id: &'static str,
    index: u32,
}

/// The deterministic steps of a member job a member settles from: the
/// preparation, the partition apply and the follow-up apply — the pilot's
/// targeted repair, or finding verification's escalation. Each is found by
/// step id and action in the definition the run executed, never by position,
/// so a step inserted before them cannot hand another step's output to
/// settlement [ORB-15197].
pub(super) struct PilotSteps {
    pub(super) prepare: Option<PilotStep>,
    pub(super) apply: Option<PilotStep>,
    pub(super) repair_apply: Option<PilotStep>,
    /// The initial apply's count of tasks the follow-up apply settles.
    pub(super) follow_up_count: &'static str,
}

impl PilotSteps {
    pub(super) fn of(
        runtime: &OrbitRuntime,
        run: &JobRun,
        kind: StateTriggerKind,
    ) -> Result<Self, AutomationError> {
        let (prepare, apply, follow_up, follow_up_count) = match kind {
            StateTriggerKind::FindingUnverified => (
                "prepare_finding_verification",
                "apply_finding_verification",
                "apply_escalations",
                "escalation_count",
            ),
            StateTriggerKind::PreparationEligible | StateTriggerKind::ExecutionFailed => (
                "prepare_task_pilot",
                "apply_task_pilot_results",
                "apply_repairs",
                "repair_count",
            ),
            // Settled from triage markers, never from an apply checkpoint.
            StateTriggerKind::PilotWarningsHeld | StateTriggerKind::BlockEscalated => {
                return Ok(Self {
                    prepare: None,
                    apply: None,
                    repair_apply: None,
                    follow_up_count: "repair_count",
                });
            }
        };
        // The run's pinned snapshot when it has one, otherwise the catalog
        // asset. A job no catalog ships any more (the retired triage job)
        // offers no step, so its runs settle as stopped without evidence.
        let mut job = match runtime.resolve_run_definition(run) {
            Ok((_, job)) => job,
            Err(OrbitError::NotFound {
                kind: NotFoundKind::Job,
                ..
            }) => {
                return Ok(Self {
                    prepare: None,
                    apply: None,
                    repair_apply: None,
                    follow_up_count,
                });
            }
            Err(error) => return Err(error.into()),
        };
        // Resolve `activity:` refs exactly as execution does, so only a step
        // that ran the deterministic action qualifies.
        let catalog = runtime
            .v2_activity_catalog()
            .map_err(|e| AutomationError::Evidence(format!("activity catalog: {e}")))?;
        orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog)
            .map_err(|e| AutomationError::Evidence(format!("run definition: {e}")))?;
        Ok(Self {
            prepare: deterministic_step(&job, "prepare", prepare),
            apply: deterministic_step(&job, "apply", apply),
            repair_apply: deterministic_step(&job, follow_up, apply),
            follow_up_count,
        })
    }
}

/// The top-level step `id` of `job`, when it runs the deterministic `action`.
pub(super) fn deterministic_step(job: &JobV2, id: &'static str, action: &str) -> Option<PilotStep> {
    let index = job.steps.iter().position(|step| step.id == id)?;
    let JobV2StepBody::Target(TargetStep {
        spec: ActivityV2Spec::Deterministic(spec),
        ..
    }) = &job.steps[index].body
    else {
        return None;
    };
    (spec.action == action).then_some(PilotStep {
        id,
        index: u32::try_from(index).ok()?,
    })
}

/// The output `step` checkpointed, once it succeeded. The pipeline entry
/// under the step's id is written by the same checkpoint; a mismatch means
/// the index names another step in what was recorded, so nothing is read.
pub(super) fn checkpoint(state: Option<&PipelineState>, step: Option<PilotStep>) -> Option<&Value> {
    let (state, step) = (state?, step?);
    if state.step_states.get(&step.index) != Some(&JobRunState::Success) {
        return None;
    }
    let output = state.step_output(step.index)?;
    state
        .pipeline
        .get(step.id)
        .is_none_or(|entry| entry == output)
        .then_some(output)
}

/// Whether `output` is an apply record: every apply output counts its
/// follow-ups and carries the member evidence slot. Anything else (a skipped
/// step's null, another step's output) settles no member [ORB-15197].
pub(super) fn is_apply_output(output: &Value) -> bool {
    output.get("repair_count").is_some_and(Value::is_u64) || output.get("member_evidence").is_some()
}

/// The `member_evidence` an apply step recorded: one entry per claim member it
/// applied. A run checkpointed before batching carried a single object.
pub(super) fn member_evidence(output: &Value) -> Result<Vec<MemberEvidence>, AutomationError> {
    match output.get("member_evidence") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|entry| serde_json::from_value(entry.clone()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AutomationError::Evidence(e.to_string())),
        Some(entry) => serde_json::from_value(entry.clone())
            .map(|evidence| vec![evidence])
            .map_err(|e| AutomationError::Evidence(e.to_string())),
    }
}
