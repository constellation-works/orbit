//! The one proposed task that asks an operator to make a concluded
//! experiment's winning overlay the default. Concluding never edits
//! configuration or repository files itself.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_store::contracts::ExperimentRecord;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskType};

use super::definition::{ExperimentOverlay, InstructionOverlay, OverlayRole};
use super::readout::ExperimentReadout;
use crate::OrbitRuntime;
use crate::application::task::ProposedTaskInput;

/// `bounded_text` refuses a description longer than this.
const DESCRIPTION_LIMIT: usize = 32_768;

impl OrbitRuntime {
    /// File the promotion task for `arm_name`, or reuse the one already filed
    /// for this same conclusion. The payload omits the readout's `as_of`, so a
    /// retry of the same decision finds the same task.
    pub(super) fn file_experiment_promotion(
        &self,
        record: &ExperimentRecord,
        arm_name: &str,
        readout: &ExperimentReadout,
    ) -> Result<String, OrbitError> {
        let experiment = &record.experiment;
        let arm = record
            .arms
            .iter()
            .find(|known| known.name == arm_name)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "experiment `{}` has no arm `{arm_name}`",
                    experiment.name
                ))
            })?;
        let overlay_value = serde_json::from_str(&arm.overlay_json).map_err(|error| {
            OrbitError::Execution(format!(
                "experiment `{}` arm `{arm_name}` overlay is not JSON: {error}",
                experiment.name
            ))
        })?;
        let overlay = ExperimentOverlay::parse(arm_name, overlay_value)?;
        let description = promotion_description(
            &experiment.id,
            &experiment.name,
            arm_name,
            readout,
            &overlay,
        )?;
        if description.len() > DESCRIPTION_LIMIT {
            return Err(OrbitError::InvalidInput(format!(
                "experiment `{}` arm `{arm_name}` overlay is too large to describe in a promotion task ({} bytes; the limit is {DESCRIPTION_LIMIT})",
                experiment.name,
                description.len()
            )));
        }
        let workspace_id = self.workspace_id()?;
        let input = ProposedTaskInput {
            request_id: format!("exp-conclude-{}", experiment.id),
            binding: sha256_hex(workspace_id.as_bytes()),
            title: format!("Promote experiment {} arm {arm_name}", experiment.name),
            description,
            complexity: TaskComplexity::Medium,
            acceptance_criteria: promotion_criteria(&experiment.name, arm_name, &overlay),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Feature),
        };
        match self.create_proposed_task(input) {
            Err(OrbitError::InvalidInput(message)) if message == "action key input changed" => {
                Err(OrbitError::InvalidInput(format!(
                    "experiment `{}` already has a promotion task for a different conclusion. Conclude again with the same arm and the same readout verdict, or remove that proposed task before choosing another",
                    experiment.name
                )))
            }
            other => other,
        }
    }
}

fn promotion_description(
    id: &str,
    name: &str,
    arm: &str,
    readout: &ExperimentReadout,
    overlay: &ExperimentOverlay,
) -> Result<String, OrbitError> {
    let overlay_json = serde_json::to_string_pretty(overlay)
        .map_err(|error| OrbitError::Execution(format!("serialize experiment overlay: {error}")))?;
    let verdict = &readout.verdict;
    let mut text = format!(
        "Promote the winning arm of experiment `{name}` ({id}) to the workspace default.\n\nArm: `{arm}`\nReadout verdict: {label}\n",
        label = verdict.label,
    );
    if verdict.reasons.is_empty() {
        text.push_str("The readout gave no reasons.\n");
    } else {
        text.push_str("Reasons:\n");
        for reason in &verdict.reasons {
            text.push_str("- ");
            text.push_str(reason);
            text.push('\n');
        }
    }
    text.push_str("\nOverlay:\n");
    text.push_str(&overlay_json);
    text.push_str("\n\nWhat to change:\n");
    let changes = promotion_changes(overlay);
    if changes.is_empty() {
        text.push_str(
            "The winning overlay is empty, so leave the current defaults in place and record that this arm won.\n",
        );
    } else {
        for change in &changes {
            text.push_str("- ");
            text.push_str(change);
            text.push('\n');
        }
    }
    Ok(text)
}

fn promotion_criteria(name: &str, arm: &str, overlay: &ExperimentOverlay) -> Vec<String> {
    if promotion_changes(overlay).is_empty() {
        vec![format!(
            "Record that arm `{arm}` of experiment `{name}` won with an empty overlay, and leave the current defaults unchanged."
        )]
    } else {
        vec![
            "Make the winning overlay the default, following every change listed in the description.".into(),
            "Change configuration or repository files only in this task's reviewed delivery. Concluding the experiment does not change them.".into(),
        ]
    }
}

fn promotion_changes(overlay: &ExperimentOverlay) -> Vec<String> {
    let mut lines = Vec::new();
    for (role, instruction) in &overlay.instructions {
        let role = role_label(*role);
        match instruction {
            InstructionOverlay::File(path) => lines.push(format!(
                "Instruction file for {role}: make `{path}` the default for that role, as a tag-instruction mapping or a workspace-guide change."
            )),
            InstructionOverlay::Suppress(false) => lines.push(format!(
                "Instructions for {role}: leave the shipped default tag instructions out of that role."
            )),
            InstructionOverlay::Suppress(true) => lines.push(format!(
                "Instructions for {role}: replace the stored suppress flag with the intended default."
            )),
        }
    }
    if let Some(skills) = &overlay.skills {
        for (skill, bundle) in &skills.replace {
            lines.push(format!(
                "Skill `{skill}`: replace the canonical skill with the bundle at `{bundle}`."
            ));
        }
    }
    if let Some(tools) = &overlay.tools {
        for tool in &tools.add {
            lines.push(format!("Tool `{tool}`: add it to the activity baseline."));
        }
        for tool in &tools.remove {
            lines.push(format!(
                "Tool `{tool}`: remove it from the activity baseline."
            ));
        }
    }
    if let Some(crew) = &overlay.crew {
        let weights = crew
            .pool
            .iter()
            .map(|member| format!("{} (weight {})", member.crew, member.weight))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Crew pool: set the complexity pool weights to {weights}."
        ));
    }
    if let Some(config) = &overlay.config {
        for (key, value) in &config.values {
            lines.push(format!("Config `{key}`: set it to {value}."));
        }
    }
    lines
}

fn role_label(role: OverlayRole) -> &'static str {
    match role {
        OverlayRole::Implement => "implement",
        OverlayRole::Review => "review",
        OverlayRole::Recovery => "recovery",
    }
}
