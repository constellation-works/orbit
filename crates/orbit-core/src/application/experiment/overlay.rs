//! Live overlays: what a live arm applies to one activity dispatch, and what
//! an owner resolves into a claim so a follower applies the same arm.
//!
//! This module resolves instructions, tools and skill bundles at dispatch; `run_policy`
//! handles crew admission and task-local config. An arm's instruction file joins the tag-instruction
//! section, read at the run's base revision on the owner and at the claim
//! base for a claimed leaf; `false` leaves Orbit's shipped default
//! instructions out. An arm's tool delta is applied after the activity's
//! tool grant. Skill files are read at the same base and carried to the
//! provider's invocation-local discovery root. A memory arm's crew memory is
//! resolved for the dispatch's own model: from the admission capture when it
//! holds that model's delivery, otherwise (on the owner only) from the
//! store. A claimed leaf applies only its receipt's deliveries. A shadow
//! experiment applies nothing.

use orbit_common::OrbitError;
use orbit_store::contracts::AdmissionShipContract;
use orbit_types::task::Task;
use orbit_types::tool::{AGENT_CONTRACT_TOOLS, is_exact_canonical_tool_name};
use orbit_types::workflow::{
    CapturedExperiment, CapturedExperimentInstruction, CapturedExperiments, CrewMemoryDelivery,
    ExperimentActivityOverlay, TagInstruction, TagInstructionError, TagInstructionRole,
    TagInstructionSource,
};
use serde_json::Value;

use super::capture::{validate_claimed_experiment_capture, validate_experiment_capture};
use super::definition::{ExperimentOverlay, InstructionOverlay, OverlayRole};
use crate::OrbitRuntime;
use crate::application::crew_memory::memory_key;
use crate::application::job::granted_tool_refusal;
use crate::application::tag_instructions::{LazyRevision, TagInstructionFailure, read_blob_at};
use crate::runtime::run_input::singular_task_id_from_input;

/// The experiment mode whose overlays apply.
pub(crate) const LIVE: &str = "live";

/// One role's instruction an arm gives a dispatch.
enum ArmInstruction {
    /// The arm sets nothing for this role.
    None,
    /// `false`: Orbit's shipped default instructions are left out.
    Suppress,
    Text(TagInstruction),
}

impl OrbitRuntime {
    /// The live arms `activity_name` of `run_id` applies for the task `input`
    /// names, in assignment order. A claimed leaf answers only from its
    /// receipt's capture and refuses without one; every other run reads the
    /// admitted capture and reads arm files at the run's base. `model` is
    /// the dispatch's own model, the key of any crew memory it receives;
    /// without one no memory is delivered.
    pub(crate) fn experiment_overlays_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
        model: Option<&str>,
    ) -> Result<Vec<ExperimentActivityOverlay>, TagInstructionFailure> {
        let Some(role) = TagInstructionRole::for_activity(activity_name) else {
            return Ok(Vec::new());
        };
        let Some(task_id) = singular_task_id_from_input(input) else {
            return Ok(Vec::new());
        };
        if self.worker_invocation().is_some() {
            return self.claimed_experiment_overlays(task_id, role, model);
        }
        // A run's admitted capture remains authoritative after lifecycle or
        // operator changes. Legacy invocations without a run capture use the
        // current effective policy, never terminal treatment.
        let run = self
            .get_job_run_backend(run_id)
            .map_err(TagInstructionFailure::Store)?;
        let capture = run
            .as_ref()
            .and_then(|run| run.input.as_ref())
            .and_then(|input| input.get(super::capture::EXPERIMENT_CAPTURE_KEY))
            .cloned()
            .map(serde_json::from_value::<CapturedExperiments>)
            .transpose()
            .map_err(|error| {
                TagInstructionFailure::Store(OrbitError::Store(format!(
                    "invalid experiment capture: {error}"
                )))
            })?
            .map(Ok)
            .unwrap_or_else(|| self.existing_task_experiment_capture(task_id))
            .map_err(TagInstructionFailure::Store)?;
        validate_experiment_capture(Some(&capture), task_id)
            .map_err(TagInstructionFailure::Store)?;
        let mut revision = LazyRevision::new(|| self.run_base_revision(run_id));
        let mut overlays = Vec::new();
        for assignment in capture.assignments.into_iter().filter(|entry| entry.live) {
            let arm = assignment.arm.clone();
            let segment = assignment.segment;
            let overlay = parse_overlay(&arm, &assignment.overlay_json)?;
            let instruction = match overlay.instructions.get(&overlay_role(role)) {
                None => ArmInstruction::None,
                Some(InstructionOverlay::Suppress(_)) => ArmInstruction::Suppress,
                Some(InstructionOverlay::File(path)) => ArmInstruction::Text(
                    self.experiment_instruction(
                        &assignment.experiment,
                        &arm,
                        path,
                        role,
                        &mut revision,
                    )
                    .map_err(TagInstructionFailure::Refused)?,
                ),
            };
            let skills = overlay
                .skills
                .as_ref()
                .filter(|skills| skills.roles.contains(&overlay_role(role)))
                .map(|skills| self.resolve_experiment_skills(skills, &mut revision))
                .transpose()
                .map_err(TagInstructionFailure::Store)?
                .unwrap_or_default();
            let memory = self
                .owner_memory_delivery(&assignment, &overlay, role, model)
                .map_err(TagInstructionFailure::Store)?;
            overlays.extend(activity_overlay(
                &assignment.experiment,
                &arm,
                segment,
                &overlay,
                role,
                ArmTreatment {
                    instruction,
                    skills,
                    memory,
                },
            ));
        }
        Ok(overlays)
    }

    /// The crew memory an owner dispatch in `role` running `model` receives
    /// from a live memory arm: the admission capture's delivery for that
    /// role and model, else a fresh rendering (a reviewer or recovery crew
    /// the admission did not draw). Nothing while `crew_memory.enabled` is
    /// off, or when the dispatch records no model.
    fn owner_memory_delivery(
        &self,
        assignment: &CapturedExperiment,
        overlay: &ExperimentOverlay,
        role: TagInstructionRole,
        model: Option<&str>,
    ) -> Result<Option<CrewMemoryDelivery>, OrbitError> {
        let Some(surface) = overlay.memory_for_role(overlay_role(role)) else {
            return Ok(None);
        };
        let Some(key) = memory_key(model) else {
            return Ok(None);
        };
        if !self.current_crew_memory_enabled() {
            return Ok(None);
        }
        if let Some(captured) = captured_memory_delivery(assignment, role, key.as_str()) {
            return Ok(Some(captured.clone()));
        }
        self.render_crew_memory_delivery(role, &key, surface.global)
            .map(Some)
    }

    /// The live arms a claimed leaf applies, from the owner's capture alone.
    fn claimed_experiment_overlays(
        &self,
        task_id: &str,
        role: TagInstructionRole,
        model: Option<&str>,
    ) -> Result<Vec<ExperimentActivityOverlay>, TagInstructionFailure> {
        let refused = |detail: String| {
            TagInstructionFailure::Refused(TagInstructionError::Capture(format!(
                "claimed leaf experiment capture refused: {detail}"
            )))
        };
        let capture = self
            .current_claimed_leaf()
            .map_err(TagInstructionFailure::Store)?
            .admission
            .receipt
            .and_then(|receipt| receipt.experiments);
        validate_claimed_experiment_capture(capture.as_ref(), task_id)
            .map_err(|error| refused(error.to_string()))?;
        let mut overlays = Vec::new();
        for entry in capture
            .iter()
            .flat_map(|capture| &capture.assignments)
            .filter(|entry| entry.live)
        {
            let overlay = parse_overlay(&entry.arm, &entry.overlay_json)?;
            let instruction = match overlay.instructions.get(&overlay_role(role)) {
                None => ArmInstruction::None,
                Some(InstructionOverlay::Suppress(_)) => ArmInstruction::Suppress,
                Some(InstructionOverlay::File(_)) => {
                    let captured = entry
                        .instructions
                        .iter()
                        .find(|captured| captured.role == role)
                        .ok_or_else(|| {
                            refused(format!(
                                "experiment `{}` arm `{}` has no {} instruction",
                                entry.experiment,
                                entry.arm,
                                role.as_str()
                            ))
                        })?;
                    if let Some(refusal) = &captured.refusal {
                        return Err(TagInstructionFailure::Refused(
                            TagInstructionError::Capture(refusal.clone()),
                        ));
                    }
                    ArmInstruction::Text(captured.instruction.clone().ok_or_else(|| {
                        refused(format!(
                            "experiment `{}` arm `{}` captured no {} text",
                            entry.experiment,
                            entry.arm,
                            role.as_str()
                        ))
                    })?)
                }
            };
            let mut skills = Vec::new();
            if let Some(selected) = overlay
                .skills
                .as_ref()
                .filter(|skills| skills.roles.contains(&overlay_role(role)))
            {
                for (name, path) in &selected.replace {
                    let bundle = entry
                        .skills
                        .iter()
                        .find(|bundle| &bundle.name == name && &bundle.path == path)
                        .ok_or_else(|| {
                            refused(format!(
                                "skill `{name}` bundle `{path}` is missing from the owner capture"
                            ))
                        })?;
                    skills.push(bundle.clone());
                }
            }
            // The owner resolved memory for the crew it drew. A dispatch
            // running another model (a follower redraw, or a reviewer crew
            // the owner did not draw) has no delivery and receives none.
            let memory = overlay
                .memory_for_role(overlay_role(role))
                .and_then(|_| memory_key(model))
                .and_then(|key| captured_memory_delivery(entry, role, key.as_str()).cloned());
            if overlay.memory_for_role(overlay_role(role)).is_some()
                && memory.is_none()
                && let Some(model) = model
            {
                tracing::warn!(
                    experiment = %entry.experiment,
                    arm = %entry.arm,
                    role = role.as_str(),
                    model,
                    "claimed leaf receives no crew memory: the owner capture holds no delivery for this role and model"
                );
            }
            overlays.extend(activity_overlay(
                &entry.experiment,
                &entry.arm,
                entry.segment,
                &overlay,
                role,
                ArmTreatment {
                    instruction,
                    skills,
                    memory,
                },
            ));
        }
        Ok(overlays)
    }

    /// The owner's capture for a claim of `task`: its assignments, with each
    /// live arm's instruction files read at the claim's base. A file that
    /// cannot be admitted is captured as a refusal, so the leaf refuses the
    /// role it applies to rather than running without it.
    pub(super) fn capture_claimed_task_experiments(
        &self,
        task: &Task,
        ship: &AdmissionShipContract,
    ) -> Result<CapturedExperiments, OrbitError> {
        let mut capture = self.capture_task_experiments(task)?;
        let mut revision = LazyRevision::new(|| self.claim_base_revision(ship));
        // Crew memory follows the crew this claim runs as, resolved on the
        // owner: the follower holds no memory tables. A crew that does not
        // resolve here records no model and receives no memory.
        let claim_model = self
            .resolve_crew_for_task(None, task.crew.as_deref())
            .ok()
            .map(|crew| crew.assignment.model);
        for entry in &mut capture.assignments {
            let overlay = ExperimentOverlay::parse(
                &entry.arm,
                serde_json::from_str(&entry.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            entry.memory = self.experiment_memory_capture(&overlay, claim_model.as_deref())?;
            if !entry.live {
                continue;
            }
            for (role, instruction) in &overlay.instructions {
                let InstructionOverlay::File(path) = instruction else {
                    continue;
                };
                let role = instruction_role(*role);
                let resolved = self.experiment_instruction(
                    &entry.experiment,
                    &entry.arm,
                    path,
                    role,
                    &mut revision,
                );
                entry.instructions.push(CapturedExperimentInstruction {
                    role,
                    refusal: resolved.as_ref().err().map(ToString::to_string),
                    instruction: resolved.ok(),
                });
            }
            if let Some(skills) = &overlay.skills {
                entry.skills = self.resolve_experiment_skills(skills, &mut revision)?;
            }
        }
        validate_experiment_capture(Some(&capture), &task.id)?;
        Ok(capture)
    }

    /// Refuse an experiment whose live surfaces cannot apply: an instruction
    /// file missing or inadmissible at the workspace's current base, an added
    /// tool a task requirement could not name, or a removed tool that is
    /// unknown or that the agent activity contract requires.
    pub(crate) fn validate_experiment_overlays(
        &self,
        name: &str,
        arms: &[(String, ExperimentOverlay)],
    ) -> Result<(), OrbitError> {
        let refuse = |detail: String| {
            OrbitError::InvalidInput(format!("experiment `{name}` cannot start: {detail}"))
        };
        let mut revision = LazyRevision::new(|| self.current_base_revision());
        for (arm, overlay) in arms {
            if let Some(skills) = &overlay.skills {
                self.resolve_experiment_skills(skills, &mut revision)
                    .map_err(|error| refuse(format!("arm `{arm}`: {error}")))?;
                self.validate_experiment_skill_providers(overlay)
                    .map_err(|error| refuse(format!("arm `{arm}`: {error}")))?;
            }
            for (role, instruction) in &overlay.instructions {
                if let InstructionOverlay::File(path) = instruction {
                    self.experiment_instruction(
                        name,
                        arm,
                        path,
                        instruction_role(*role),
                        &mut revision,
                    )
                    .map_err(|error| refuse(format!("arm `{arm}`: {error}")))?;
                }
            }
            let Some(tools) = &overlay.tools else {
                continue;
            };
            for tool in &tools.add {
                if let Some(reason) = granted_tool_refusal(self, tool)? {
                    return Err(refuse(format!("arm `{arm}` adds tool `{tool}`: {reason}")));
                }
            }
            for tool in &tools.remove {
                let reason = if AGENT_CONTRACT_TOOLS.contains(&tool.as_str()) {
                    Some("the agent activity contract requires it")
                } else if !is_exact_canonical_tool_name(tool) {
                    Some("malformed canonical tool name")
                } else if !self.tool_registry().has(tool) {
                    Some("unknown registered tool")
                } else if !self.tool_registry().is_active(tool) {
                    Some("tool is not agent-facing")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(refuse(format!(
                        "arm `{arm}` removes tool `{tool}`: {reason}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// An arm's instruction file for `role`, read at `revision` and admitted
    /// under the tag-instruction rules.
    fn experiment_instruction(
        &self,
        experiment: &str,
        arm: &str,
        path: &str,
        role: TagInstructionRole,
        revision: &mut LazyRevision<'_>,
    ) -> Result<TagInstruction, TagInstructionError> {
        let unreadable = |reason: String| TagInstructionError::Unreadable {
            tag: experiment.to_string(),
            role: role.as_str(),
            origin: path.to_string(),
            reason,
        };
        let revision = revision.get().map_err(unreadable)?;
        let bytes = read_blob_at(&self.paths().repo_root, &revision, path)
            .map_err(|reason| unreadable(format!("at {revision}: {reason}")))?;
        TagInstruction::admit(
            experiment,
            role,
            TagInstructionSource::Experiment {
                experiment: experiment.to_string(),
                arm: arm.to_string(),
                path: path.to_string(),
                revision,
            },
            path,
            &bytes,
        )
    }

    /// The workspace's current base: its base branch as last fetched from
    /// `origin`, else the local branch, else this checkout's `HEAD`.
    fn current_base_revision(&self) -> Result<String, String> {
        self.workspace_base_revision()
    }
}

/// What an arm resolved for one dispatch: its instruction, skills and crew
/// memory.
struct ArmTreatment {
    instruction: ArmInstruction,
    skills: Vec<orbit_types::workflow::ExperimentSkillBundle>,
    memory: Option<CrewMemoryDelivery>,
}

/// What `overlay` gives a dispatch in `role`, or `None` when it changes
/// nothing there. Tools apply only to the roles their entry names.
fn activity_overlay(
    experiment: &str,
    arm: &str,
    segment: u32,
    overlay: &ExperimentOverlay,
    role: TagInstructionRole,
    treatment: ArmTreatment,
) -> Option<ExperimentActivityOverlay> {
    let ArmTreatment {
        instruction,
        skills,
        memory,
    } = treatment;
    let tools = overlay
        .tools
        .as_ref()
        .filter(|tools| tools.roles.contains(&overlay_role(role)));
    let (instruction, suppress_default_instructions) = match instruction {
        ArmInstruction::None => (None, false),
        ArmInstruction::Suppress => (None, true),
        ArmInstruction::Text(text) => (Some(text), false),
    };
    if instruction.is_none()
        && !suppress_default_instructions
        && tools.is_none()
        && skills.is_empty()
        && memory.is_none()
    {
        return None;
    }
    Some(ExperimentActivityOverlay {
        experiment: experiment.to_string(),
        arm: arm.to_string(),
        segment,
        role,
        instruction,
        suppress_default_instructions,
        tools_add: tools.map(|tools| tools.add.clone()).unwrap_or_default(),
        tools_remove: tools.map(|tools| tools.remove.clone()).unwrap_or_default(),
        skills,
        memory,
    })
}

/// The captured crew memory delivery for `role` and memory key `model`.
fn captured_memory_delivery<'a>(
    entry: &'a CapturedExperiment,
    role: TagInstructionRole,
    model: &str,
) -> Option<&'a CrewMemoryDelivery> {
    entry
        .memory
        .iter()
        .filter(|memory| memory.enabled)
        .flat_map(|memory| &memory.deliveries)
        .find(|delivery| delivery.role == role && delivery.model == model)
}

fn parse_overlay(arm: &str, json: &str) -> Result<ExperimentOverlay, TagInstructionFailure> {
    serde_json::from_str(json)
        .map_err(|error| OrbitError::Store(format!("invalid experiment overlay: {error}")))
        .and_then(|value| ExperimentOverlay::parse(arm, value))
        .map_err(TagInstructionFailure::Store)
}

fn overlay_role(role: TagInstructionRole) -> OverlayRole {
    match role {
        TagInstructionRole::Implement => OverlayRole::Implement,
        TagInstructionRole::Review => OverlayRole::Review,
        TagInstructionRole::Recovery => OverlayRole::Recovery,
    }
}

pub(super) fn instruction_role(role: OverlayRole) -> TagInstructionRole {
    match role {
        OverlayRole::Implement => TagInstructionRole::Implement,
        OverlayRole::Review => TagInstructionRole::Review,
        OverlayRole::Recovery => TagInstructionRole::Recovery,
    }
}
