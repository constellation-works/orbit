//! Fail-closed experiment captures: the arms a run applies, carried in its
//! input on the owner and in the admission receipt for a claimed leaf.
use super::definition::{ExperimentOverlay, overlay_digest};
use crate::OrbitRuntime;
use orbit_common::OrbitError;
use orbit_store::contracts::ExperimentStoreBackend as _;
use orbit_types::{
    task::Task,
    workflow::{CapturedExperiment, CapturedExperiments},
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
            validate_experiment_capture(capture.as_ref(), task_id)?;
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
                live: record.experiment.mode == super::overlay::LIVE,
                instructions: Vec::new(),
                skills: Vec::new(),
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
        let capture = if self
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
        input[EXPERIMENT_CAPTURE_KEY] = serde_json::to_value(capture).map_err(|error| {
            OrbitError::Execution(format!("serialize experiment capture: {error}"))
        })?;
        Ok(())
    }
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
