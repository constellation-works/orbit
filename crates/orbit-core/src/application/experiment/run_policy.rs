//! Crew admission and task-local workflow values from the frozen arm capture.

use orbit_common::OrbitError;
use orbit_config::{CrewPoolEntry, ExperimentConfigKey, canonical_crew_pool_entries};
use orbit_types::workflow::{CapturedExperiments, TagInstructionRole};
use serde_json::Value;

use super::capture::{EXPERIMENT_CAPTURE_KEY, validate_experiment_capture};
use super::definition::{ExperimentOverlay, OverlayRole};
use crate::OrbitRuntime;

pub(crate) struct ExperimentCrewPool {
    pub(crate) entries: Vec<CrewPoolEntry>,
    pub(crate) source: String,
    pub(crate) digest: String,
    /// The experiment and arm whose overlay this pool is, which a crew
    /// written back to the task names.
    pub(crate) experiment: String,
    pub(crate) arm: String,
}

impl OrbitRuntime {
    /// Freeze a claimed leaf's treatment once on the executing host. Its
    /// pipeline state retains both the draw and any availability deviation.
    pub(crate) fn install_claimed_experiment_crew(
        &self,
        input: &mut Value,
    ) -> Result<(), OrbitError> {
        let leaf = self.current_claimed_leaf()?;
        let task = leaf
            .admission
            .receipt
            .and_then(|receipt| receipt.task)
            .ok_or_else(|| OrbitError::PolicyDenied("claimed task snapshot missing".into()))?;
        if input.get("crew_selection").is_some() || self.experiment_crew_pool(&task.id)?.is_none() {
            return Ok(());
        }
        let now = chrono::Utc::now();
        let view = orbit_types::task::Task {
            id: task.id,
            title: task.title,
            description: String::new(),
            acceptance_criteria: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            granted_tools: Vec::new(),
            requires_labels: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: task.context_files,
            created_by: None,
            planned_by: None,
            implemented_by: None,
            status: orbit_types::task::TaskStatus::Backlog,
            priority: orbit_types::task::TaskPriority::Medium,
            complexity: task.complexity,
            task_type: orbit_types::task::TaskType::Chore,
            pr_status: None,
            external_refs: Vec::new(),
            relations: Vec::new(),
            job_run_id: None,
            job_run_machine: None,
            crew: None,
            crew_source: Some(format!(
                "pool:{}",
                task.complexity.map_or("medium", |tier| tier.as_str())
            )),
            orchestrator: None,
            orchestrator_model: None,
            orchestrator_model_source: None,
            created_by_model: None,
            origin: None,
            created_at: now,
            updated_at: now,
            not_before: None,
        };
        input["auto_crew_pools"] = serde_json::to_value(self.auto_crew_pools_for_input(input)?)
            .map_err(|error| OrbitError::Execution(format!("serialize crew pools: {error}")))?;
        // The owner writes the leaf's crew back to its task when it settles.
        let writes = self.capture_task_crew_selection(
            input,
            &view,
            &mut crate::application::job::crew_pools::random_crew_ticket,
        )?;
        self.apply_admission_task_writes(writes.without_crew_write());
        Ok(())
    }

    /// Existing assignments only: eligibility must never enroll a task.
    pub(crate) fn experiment_crew_pool(
        &self,
        task_id: &str,
    ) -> Result<Option<ExperimentCrewPool>, OrbitError> {
        let capture = self.existing_task_experiment_capture(task_id)?;
        for entry in capture.assignments.iter().filter(|entry| entry.live) {
            let overlay = ExperimentOverlay::parse(
                &entry.arm,
                serde_json::from_str(&entry.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            if let Some(crew) = overlay.crew {
                let source = format!("experiment:{}:{}", entry.experiment, entry.arm);
                let entries = crew
                    .pool
                    .into_iter()
                    .map(|member| CrewPoolEntry {
                        name: member.crew,
                        weight: member.weight,
                    })
                    .collect::<Vec<_>>();
                let entries = canonical_crew_pool_entries(
                    &entries,
                    self.context.settings().crews(),
                    &source,
                )?;
                return Ok(Some(ExperimentCrewPool {
                    entries,
                    source,
                    digest: entry.digest.clone(),
                    experiment: entry.experiment.clone(),
                    arm: entry.arm.clone(),
                }));
            }
        }
        Ok(None)
    }

    /// Crews have no per-complexity restriction: every enabled, resolvable
    /// registry member may run all population tiers. Host filters still apply
    /// at admission, where the executing host is known.
    pub(crate) fn validate_experiment_crews(
        &self,
        name: &str,
        arms: &[(String, ExperimentOverlay)],
    ) -> Result<(), OrbitError> {
        for (arm, overlay) in arms {
            if let Some(crew) = &overlay.crew {
                let entries = crew
                    .pool
                    .iter()
                    .map(|member| CrewPoolEntry {
                        name: member.crew.clone(),
                        weight: member.weight,
                    })
                    .collect::<Vec<_>>();
                let source = format!("experiment `{name}` arm `{arm}` crew pool");
                canonical_crew_pool_entries(&entries, self.context.settings().crews(), &source)?;
                for member in &entries {
                    self.resolve_crew_for_task(Some(&member.name), None)?;
                }
            }
        }
        Ok(())
    }

    /// Read a typed value from this run's admission, never a sibling's or a
    /// follower's local experiments. An absent overlay inherits `fallback`.
    pub(crate) fn experiment_explore_max_calls(
        &self,
        input: &Value,
        task_id: &str,
        activity: &str,
        fallback: u32,
    ) -> Result<u32, OrbitError> {
        let capture = if self.worker_invocation().is_some() {
            Some(self.existing_task_experiment_capture(task_id)?)
        } else {
            input
                .get(EXPERIMENT_CAPTURE_KEY)
                .cloned()
                .map(serde_json::from_value::<CapturedExperiments>)
                .transpose()
                .map_err(|error| {
                    OrbitError::PolicyDenied(format!("invalid experiment capture: {error}"))
                })?
        };
        let Some(capture) = capture else {
            return Ok(fallback);
        };
        validate_experiment_capture(Some(&capture), task_id)?;
        let Some(role) = TagInstructionRole::for_activity(activity) else {
            return Ok(fallback);
        };
        let role = match role {
            TagInstructionRole::Implement => OverlayRole::Implement,
            TagInstructionRole::Review => OverlayRole::Review,
            TagInstructionRole::Recovery => OverlayRole::Recovery,
        };
        let key = ExperimentConfigKey::ExploreMaxCallsPerRun;
        for entry in capture.assignments.iter().filter(|entry| entry.live) {
            let overlay = ExperimentOverlay::parse(
                &entry.arm,
                serde_json::from_str(&entry.overlay_json).map_err(|error| {
                    OrbitError::Store(format!("invalid experiment overlay: {error}"))
                })?,
            )?;
            if let Some(config) = overlay.config.filter(|config| config.roles.contains(&role))
                && let Some(value) = config.values.get(key.config_key())
            {
                key.validate_experiment_value(value)?;
                return serde_json::from_value(value.clone()).map_err(|error| {
                    OrbitError::PolicyDenied(format!("invalid experiment config: {error}"))
                });
            }
        }
        Ok(fallback)
    }
}
