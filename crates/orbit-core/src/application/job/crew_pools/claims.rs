//! Fresh owner claim routing over historical experiment crew assignments.
use super::draw::weighted_draw;
use crate::OrbitRuntime;
use crate::application::job::JobExperimentPort;
use orbit_common::OrbitError;
use orbit_types::task::Task;
use serde_json::Value;

impl OrbitRuntime {
    /// Resolve a fresh claim through the same candidates as local admission.
    /// This transient view is screened and frozen by the owner, so a stale
    /// delivery write-back cannot become a follower's new standing pin.
    pub(crate) fn fresh_claim_crew_task(
        &self,
        task: &Task,
        request: &orbit_store::contracts::AdmissionRequest,
    ) -> Result<Task, OrbitError> {
        // Preserve unrelated routing, including human work and standing
        // purpose/type assignments. Only historical crew enrollment needs
        // the fresh-authority projection.
        let capture = self.job_existing_experiment_capture(&task.id)?;
        if self.is_human_crew(task.crew.as_deref())
            || !capture.assignments.iter().any(|entry| {
                serde_json::from_str::<Value>(&entry.overlay_json)
                    .ok()
                    .is_some_and(|overlay| !overlay["crew"].is_null())
            })
        {
            return Ok(task.clone());
        }
        let pools = self.auto_crew_pools_for_input(&Value::Null)?;
        let (candidates, source) = self.auto_task_crew_candidates(task, &pools, None, None)?;
        let runnable = candidates
            .iter()
            .filter(|candidate| {
                request.crews.as_ref().is_none_or(|crews| {
                    crews
                        .unrunnable_reason(Some(&candidate.crew.name))
                        .is_none()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        // When no member can run, retain the effective pool's candidate so
        // the store reports crew_unavailable instead of silently widening it.
        let candidates = if runnable.is_empty() {
            candidates
        } else {
            runnable
        };
        let mut ticket =
            super::draw::seeded_crew_ticket(format!("claim:{}:{}", request.request_id, task.id));
        let selected = weighted_draw(&candidates, &source, &mut ticket)?;
        let mut view = task.clone();
        view.crew = Some(selected.crew.name.clone());
        if source != "task.crew" {
            view.crew_source = Some(source);
        }
        Ok(view)
    }
}
