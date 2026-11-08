//! Fail-closed experiment captures: the arms a run applies, carried in its
//! input on the owner and in the admission receipt for a claimed leaf.
use super::definition::{ExperimentOverlay, overlay_digest};
use super::overlay::instruction_role;
use crate::OrbitRuntime;
use crate::application::crew_memory::memory_key;
use orbit_common::OrbitError;
use orbit_store::contracts::ExperimentStoreBackend as _;
use orbit_types::{
    task::Task,
    workflow::{CapturedExperiment, CapturedExperimentMemory, CapturedExperiments},
};
use serde_json::Value;

const CAPTURE_LIMIT: usize = 256 * 1024;
pub(crate) const EXPERIMENT_CAPTURE_KEY: &str = "experiment_capture";

impl OrbitRuntime {
    pub(crate) fn capture_task_experiments(
        &self,
        task: &Task,
    ) -> Result<CapturedExperiments, OrbitError> {
        let rows = self.admit_task_experiments(task)?;
        self.capture_experiment_assignments(&task.id, rows)
    }

    /// Read existing assignments without enrolling during eligibility checks.
    pub(crate) fn existing_task_experiment_capture(
        &self,
        task_id: &str,
    ) -> Result<CapturedExperiments, OrbitError> {
        if self.worker_invocation().is_some() {
            let capture = self
                .current_claimed_leaf()?
                .admission
                .receipt
                .and_then(|receipt| receipt.experiments);
            validate_claimed_experiment_capture(capture.as_ref(), task_id)?;
            return capture
                .ok_or_else(|| OrbitError::PolicyDenied("missing experiment capture".into()));
        }
        self.capture_experiment_assignments(task_id, self.task_experiments(task_id)?)
    }

    fn capture_experiment_assignments(
        &self,
        task_id: &str,
        rows: Vec<orbit_store::contracts::ExperimentAssignment>,
    ) -> Result<CapturedExperiments, OrbitError> {
        let task = self.get_task(task_id)?;
        let explicit = self.task_crew_source(&task)?.as_deref() == Some("explicit");
        let explicit_at = if explicit {
            self.get_task_history(task_id)?
                .into_iter()
                .filter(|entry| entry.event == "crew_assigned")
                .map(|entry| entry.at)
                .max()
        } else {
            None
        };
        let mut assignments = Vec::new();
        for row in rows {
            let Some(arm_name) = row.arm else {
                continue;
            };
            let record = self
                .sqlite_store()?
                .experiment_record(&self.workspace_id()?, &row.experiment)?
                .ok_or_else(|| OrbitError::Store("assigned experiment is missing".into()))?;
            let arm = record
                .arms
                .iter()
                .find(|arm| arm.name == arm_name)
                .ok_or_else(|| OrbitError::Store("assigned experiment arm is missing".into()))?;
            // Enrollment is historical evidence, not perpetual execution
            // authority. Preserve paused/retired behavior, but terminal
            // experiments cannot authorize another delivery. A later audited
            // task pin also supersedes a forced assignment (ties keep force).
            let overlay = ExperimentOverlay::parse(
                &arm_name,
                serde_json::from_str(&arm.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            let task_override = explicit
                && overlay.crew.is_some()
                && (!row.forced || explicit_at.is_some_and(|at| at > row.assigned_at));
            assignments.push(CapturedExperiment {
                experiment: row.experiment,
                arm: arm_name,
                segment: row
                    .segment
                    .ok_or_else(|| OrbitError::Store("assigned segment is missing".into()))?,
                forced: row.forced,
                digest: row
                    .digest
                    .ok_or_else(|| OrbitError::Store("assigned digest is missing".into()))?,
                overlay_json: arm.overlay_json.clone(),
                live: record.experiment.mode == super::overlay::LIVE
                    && !matches!(record.experiment.status.as_str(), "concluded" | "abandoned")
                    && !task_override,
                instructions: Vec::new(),
                skills: Vec::new(),
                memory: None,
            });
        }
        let capture = CapturedExperiments {
            task_id: task_id.to_string(),
            assignments,
        };
        validate_experiment_capture(Some(&capture), task_id)?;
        Ok(capture)
    }

    pub(crate) fn install_experiment_capture(&self, input: &mut Value) -> Result<(), OrbitError> {
        let Some(task_id) =
            crate::runtime::run_input::singular_task_id_from_input(input).map(str::to_owned)
        else {
            return Ok(());
        };
        // Claimed leaves never consult replica experiments, including on resume.
        let mut capture = if self
            .worker_invocation()
            .is_some_and(|binding| binding.task_id == task_id)
        {
            let leaf = self.current_claimed_leaf()?;
            leaf.admission
                .receipt
                .and_then(|receipt| receipt.experiments)
                .ok_or_else(|| {
                    OrbitError::PolicyDenied("claimed leaf experiment capture is missing".into())
                })?
        } else {
            self.capture_task_experiments(&self.get_task(&task_id)?)?
        };
        // An explicit run crew supersedes a crew-bearing treatment as a
        // whole: never deliver only part of an arm and count it as treated.
        if input
            .get("crew")
            .and_then(Value::as_str)
            .is_some_and(|crew| !crew.trim().is_empty())
        {
            for entry in &mut capture.assignments {
                let overlay: ExperimentOverlay = serde_json::from_str(&entry.overlay_json)
                    .map_err(|error| {
                        OrbitError::Store(format!("invalid experiment overlay: {error}"))
                    })?;
                if overlay.crew.is_some() {
                    entry.live = false;
                }
            }
        }
        input[EXPERIMENT_CAPTURE_KEY] = serde_json::to_value(capture).map_err(|error| {
            OrbitError::Execution(format!("serialize experiment capture: {error}"))
        })?;
        Ok(())
    }

    /// A memory arm's capture for a crew running `model`: each role in the
    /// arm's `roles` rendered from that model's newest revision (and the
    /// global's, when the arm asks). `None` for an arm without a memory
    /// surface. No recorded model, or `crew_memory.enabled` off, captures no
    /// delivery: no memory is guessed and none reaches a disabled workspace.
    pub(crate) fn experiment_memory_capture(
        &self,
        overlay: &ExperimentOverlay,
        model: Option<&str>,
    ) -> Result<Option<CapturedExperimentMemory>, OrbitError> {
        let Some(memory) = &overlay.memory else {
            return Ok(None);
        };
        let enabled = self.current_crew_memory_enabled();
        let mut deliveries = Vec::new();
        if enabled && let Some(key) = memory_key(model) {
            for role in &memory.roles {
                deliveries.push(self.render_crew_memory_delivery(
                    instruction_role(*role),
                    &key,
                    memory.global,
                )?);
            }
        }
        Ok(Some(CapturedExperimentMemory {
            enabled,
            deliveries,
        }))
    }

    /// Resolve crew memory into the run's capture after its crew draw, for
    /// the drawn crew's `model` [crew memory §5.2]. Live and shadow memory
    /// arms are both captured, so a shadow run records the digest it would
    /// have delivered. An entry that already carries memory (inherited from
    /// a coordinator that drew the same crew) is kept. A claimed leaf never
    /// resolves: it applies only what its owner's receipt carries.
    pub(crate) fn install_experiment_memory_capture(
        &self,
        input: &mut Value,
        model: Option<&str>,
    ) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Ok(());
        }
        let Some(raw) = input.get(EXPERIMENT_CAPTURE_KEY).cloned() else {
            return Ok(());
        };
        let mut capture: CapturedExperiments = serde_json::from_value(raw)
            .map_err(|error| OrbitError::Store(format!("invalid experiment capture: {error}")))?;
        let mut changed = false;
        for entry in &mut capture.assignments {
            if entry.memory.is_some() {
                continue;
            }
            let overlay = ExperimentOverlay::parse(
                &entry.arm,
                serde_json::from_str(&entry.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            entry.memory = self.experiment_memory_capture(&overlay, model)?;
            changed |= entry.memory.is_some();
        }
        if !changed {
            return Ok(());
        }
        validate_experiment_capture(Some(&capture), &capture.task_id)?;
        input[EXPERIMENT_CAPTURE_KEY] = serde_json::to_value(capture).map_err(|error| {
            OrbitError::Execution(format!("serialize experiment capture: {error}"))
        })?;
        Ok(())
    }
}

/// [`validate_experiment_capture`] for a claimed leaf, which also refuses a
/// memory arm whose owner capture carries no crew memory resolution: the
/// follower holds no memory tables and never resolves memory itself.
pub(crate) fn validate_claimed_experiment_capture(
    capture: Option<&CapturedExperiments>,
    task: &str,
) -> Result<(), OrbitError> {
    validate_experiment_capture(capture, task)?;
    for entry in capture.iter().flat_map(|capture| &capture.assignments) {
        let memory_arm = serde_json::from_str::<Value>(&entry.overlay_json)
            .ok()
            .is_some_and(|overlay| !overlay["memory"].is_null());
        if memory_arm && entry.memory.is_none() {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed leaf experiment capture refused: experiment `{}` arm `{}` delivers crew memory and the owner capture carries none",
                entry.experiment, entry.arm
            )));
        }
    }
    Ok(())
}

/// A missing capture is different from an explicitly empty owner capture.
/// Validate at claimed-leaf authorization, before any dispatch can read config.
pub(crate) fn validate_experiment_capture(
    capture: Option<&CapturedExperiments>,
    task: &str,
) -> Result<(), OrbitError> {
    let refused = |detail: &str| {
        OrbitError::PolicyDenied(format!("claimed leaf experiment capture refused: {detail}"))
    };
    let capture =
        capture.ok_or_else(|| refused("missing owner capture; no replica fallback is allowed"))?;
    if capture.task_id != task {
        return Err(refused("task identity mismatch"));
    }
    let bytes = serde_json::to_vec(capture).map_err(|_| refused("unreadable capture"))?;
    if bytes.len() > CAPTURE_LIMIT {
        return Err(refused("capture exceeds 256 KiB limit"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in &capture.assignments {
        if entry.experiment.is_empty()
            || entry.arm.is_empty()
            || entry.segment == 0
            || !seen.insert(&entry.experiment)
        {
            return Err(refused("invalid or duplicate assignment"));
        }
        let overlay = ExperimentOverlay::parse(
            &entry.arm,
            serde_json::from_str(&entry.overlay_json).map_err(|_| refused("unreadable overlay"))?,
        )
        .map_err(|_| refused("invalid overlay"))?;
        // Older shadow experiments could observe keys that were not live
        // surfaces. They still apply nothing; enforce typed values only when
        // the captured arm can actually change a run.
        if entry.live
            && let Some(config) = &overlay.config
        {
            for (key, value) in &config.values {
                orbit_config::ExperimentConfigKey::from_config_key(key)?
                    .validate_experiment_value(value)?;
            }
        }
        if overlay.canonical_json()? != entry.overlay_json
            || overlay_digest(&entry.overlay_json) != entry.digest
        {
            return Err(refused("overlay digest mismatch"));
        }
        if let Some(memory) = &entry.memory {
            let surface = overlay.memory.as_ref().ok_or_else(|| {
                refused("crew memory captured for an arm without a memory surface")
            })?;
            let mut roles = std::collections::BTreeSet::new();
            for delivery in &memory.deliveries {
                delivery.verify().map_err(|reason| refused(&reason))?;
                if !memory.enabled
                    || delivery.global != surface.global
                    || !surface
                        .roles
                        .iter()
                        .any(|role| super::overlay::instruction_role(*role) == delivery.role)
                    || !roles.insert(delivery.role)
                {
                    return Err(refused("unexpected or duplicate crew memory delivery"));
                }
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for bundle in &entry.skills {
            bundle.verify().map_err(|reason| refused(&reason))?;
            super::skills::validate_skill_manifest(bundle).map_err(|reason| refused(&reason))?;
            if !names.insert(&bundle.name)
                || !entry.live
                || overlay
                    .skills
                    .as_ref()
                    .and_then(|skills| skills.replace.get(&bundle.name))
                    != Some(&bundle.path)
            {
                return Err(refused("unexpected or duplicate skill bundle"));
            }
        }
    }
    Ok(())
}
