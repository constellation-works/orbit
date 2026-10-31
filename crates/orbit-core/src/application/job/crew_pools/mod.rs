//! Draw a task's crew from the complexity pools when it is created
//! [ORB-12717], and capture pool policy on the admitting pipeline so each
//! task's selection is frozen in its run input. Descendant pipelines inherit
//! the same task choice; a task created without a crew before assignment moved
//! to creation time, a stale pool assignment after rerating, or a default
//! fallback for an empty pool, is routed through the current pools at
//! admission. Whatever crew admission chooses is written back to the task
//! ([`write_back`], ORB-19416), so `task.crew` names the crew of its latest
//! delivery. One older write is an
//! auto-task task whose explicit crew is provider-limited [ORB-17638]:
//! admission durably reassigns that task to its complexity pool and records
//! a warning comment on it. The other is `workflow.crew_selection =
//! "usage_pressure"` [ORB-17770]: creation, re-rate and admission draws weigh
//! each member by its crew's weekly usage pressure, and admission draws a
//! system-chosen crew again from its whole pool, persisting a changed crew
//! under the task's existing automatic source. Activity, review, verifier and
//! explorer pools use the same helper through `draw_crew_pool`, after their
//! existing membership filters, and carry each member's draw provenance.
//! `workflow.crew_performance` multiplies the creation, re-rate and
//! admission draws by each crew's performance factor from the persisted
//! snapshot through the same seam; it never re-draws an assigned crew by
//! itself, and activity draws take no performance factor. `shadow_absolute`
//! and `shadow_comparative` compute and record that factor with
//! `applied: false` and leave the tickets on the weights the draw would
//! use with the mode off; pressure still applies.
//! `workflow.type_complexity_crews` gives a task type its own pool per tier
//! ([`task_type`]), drawn wherever the complexity pool would be, behind an
//! explicit crew and a purpose pool, under the same filters and weights.

mod activity;
mod admission;
mod candidates;
mod draw;
mod purpose;
#[cfg(feature = "test-support")]
mod selection_checkpoint;
mod task_type;
mod types;
mod write_back;

pub(crate) use admission::{
    AdmissionTaskWrites, crew_admission_task_id, strip_auto_crew_admission,
};
pub(crate) use draw::{
    INSPECTION_CREW_UNAVAILABLE, complexity_crews_key, complexity_of_crews_key, random_crew_ticket,
    seeded_crew_ticket,
};
#[cfg(feature = "test-support")]
pub use selection_checkpoint::with_crew_selection_checkpoint;
pub(crate) use types::CapturedCrewPools;
#[cfg(feature = "test-support")]
pub use write_back::with_crew_backfill_checkpoint;
pub use write_back::{TaskCrewBackfill, TaskCrewChange};

impl crate::OrbitRuntime {
    /// Positive-weight complexity and type pool members, before host filters.
    pub(crate) fn pool_health_crew_names(
        &self,
        input: &serde_json::Value,
    ) -> Result<std::collections::BTreeSet<String>, orbit_common::OrbitError> {
        let mut pools = self.auto_crew_pools_for_input(input)?;
        // Pull coordinators do not capture local task-draw policy. Their
        // configured pools still need health observations.
        if pools.is_empty() {
            pools = self.capture_auto_crew_pools(&serde_json::Value::Null)?;
        }
        let mut names = pools
            .values()
            .flat_map(|pool| &pool.crews)
            .filter(|entry| entry.weight > 0)
            .map(|entry| entry.name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let settings = self.context.settings();
        for (task_type, tier, entries) in settings.type_complexity_crews().iter() {
            let pool = orbit_config::canonical_crew_pool(
                entries,
                settings.crews(),
                &orbit_config::type_complexity_crews_key(task_type, tier),
            )?;
            names.extend(
                pool.entries
                    .into_iter()
                    .filter(|entry| entry.weight > 0)
                    .map(|entry| entry.name),
            );
        }
        Ok(names)
    }
}
