//! What kind of work a delivery is, captured once so its resource history
//! stays under one profile.
//!
//! The work class comes from the task's tags at admission:
//! `auto-task:<template>` from the `auto-task:` tag, else `no-diff` for
//! `no-diff-expected`, else `change`. A delivery is a chain of runs
//! (`task_auto_pipeline`, then `task_gate_pipeline`, then `task_pr_pipeline`),
//! so the first submission naming the task records the class in its run input
//! and every child that names the same task inherits it. A later tag edit
//! therefore never moves a delivery's minutes between profiles. A claimed
//! leaf is created by the pull store, which carries its admission reservation's
//! class into the queued run. Without a reservation, its worker captures the
//! class when it starts; the leaf is the whole delivery.

use chrono::{DateTime, Utc};
use orbit_types::task::Task;
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::claimed::CLAIMED_LEAF_JOBS;
pub(crate) use crate::runtime::host_resource::delivery::DeliveryResourceClass;
pub(crate) use crate::runtime::host_resource::delivery::work_class;
use crate::runtime::host_resource::delivery::{
    ATTRIBUTED_CHILD_JOBS, RESOURCE_CLASS_KEY, SUBMITTED_DELIVERY_JOBS, resource_class_in,
};
use crate::runtime::run_input::singular_task_id_from_input;

/// Run-input key of a repair claim's candidate (`orbit-store` pull leaf).
const CLAIM_REPAIR_KEY: &str = "claim_repair";

impl OrbitRuntime {
    /// Record the delivery's class in a delivery pipeline's input. A child
    /// naming its parent's task inherits the parent's class; any other
    /// submission captures it from the task now, replacing whatever the
    /// caller put under the key. `retried` marks a resume or replay, which
    /// keeps the class it carries but is excluded from profiles.
    ///
    /// Telemetry only: a task that cannot be read leaves the key unset, and
    /// the worker captures the class when it starts.
    pub(crate) fn install_resource_class_admission(
        &self,
        job_name: &str,
        input: &mut Value,
        parent_run_id: Option<&str>,
        retried: bool,
    ) {
        if !SUBMITTED_DELIVERY_JOBS.contains(&job_name) {
            return;
        }
        let Some(task_id) = singular_task_id_from_input(input).map(str::to_owned) else {
            if let Some(object) = input.as_object_mut() {
                object.remove(RESOURCE_CLASS_KEY);
            }
            return;
        };
        let inherited = parent_run_id
            .and_then(|run_id| self.get_job_run_backend(run_id).ok().flatten())
            .and_then(|parent| parent.input)
            .and_then(|parent| resource_class_in(&parent, &task_id));
        let carried = retried
            .then(|| resource_class_in(input, &task_id))
            .flatten();
        let class = inherited.or(carried).or_else(|| {
            self.get_task(&task_id)
                .ok()
                .map(|task| capture(&task, None, Utc::now()))
        });
        let Some(object) = input.as_object_mut() else {
            return;
        };
        match class.and_then(|mut class| {
            class.resumed |= retried;
            serde_json::to_value(class).ok()
        }) {
            Some(value) => {
                object.insert(RESOURCE_CLASS_KEY.to_string(), value);
            }
            None => {
                object.remove(RESOURCE_CLASS_KEY);
            }
        }
    }

    /// The class this worker's run samples under, or `None` when the run is
    /// not a delivery. Falls back to capturing it now when admission did not
    /// (a claimed leaf, or a run submitted before admission captured it).
    pub(crate) fn delivery_resource_class(
        &self,
        run: &JobRun,
        adopted: bool,
    ) -> Option<DeliveryResourceClass> {
        let job = run.job_id.as_str();
        if ATTRIBUTED_CHILD_JOBS.contains(&job) {
            // Only the class its admission copied from the delegating
            // delivery: an explorer captures none of its own.
            return run
                .input
                .as_ref()
                .and_then(|input| input.get(RESOURCE_CLASS_KEY))
                .and_then(|class| serde_json::from_value(class.clone()).ok());
        }
        let claimed = CLAIMED_LEAF_JOBS.contains(&job);
        if !claimed && !SUBMITTED_DELIVERY_JOBS.contains(&job) {
            return None;
        }
        let input = run.input.as_ref()?;
        let task_id = singular_task_id_from_input(input)?;
        let mut class = match resource_class_in(input, task_id) {
            Some(class) => class,
            None => {
                let claimed_complexity = claimed
                    .then(|| self.claimed_leaf_admission(&run.run_id).ok().flatten())
                    .flatten()
                    .and_then(|admission| admission.receipt)
                    .and_then(|receipt| receipt.task)
                    .and_then(|task| task.complexity);
                match self.get_task(task_id) {
                    Ok(task) => capture(&task, claimed_complexity, run.created_at),
                    // A claimed task lives in the owner's store; this
                    // replica may not hold it yet.
                    Err(_) => DeliveryResourceClass {
                        task_id: task_id.to_string(),
                        work_class: work_class(&[]),
                        complexity: claimed_complexity.map(|value| value.to_string()),
                        admitted_at: run.created_at,
                        resumed: false,
                    },
                }
            }
        };
        let continued_candidate = input
            .get("resume_candidate")
            .is_some_and(|candidate| !candidate.is_null())
            || input.get(CLAIM_REPAIR_KEY).is_some();
        let checkpointed = self
            .read_run_state(&run.run_id)
            .ok()
            .flatten()
            .is_some_and(|state| {
                state
                    .step_states
                    .values()
                    .any(|step| *step == JobRunState::Success)
            });
        class.resumed |= adopted
            || run.retry_source_run_id.is_some()
            || run.attempt > 1
            || continued_candidate
            || checkpointed;
        Some(class)
    }
}

fn capture(
    task: &Task,
    complexity: Option<orbit_types::task::TaskComplexity>,
    admitted_at: DateTime<Utc>,
) -> DeliveryResourceClass {
    DeliveryResourceClass {
        task_id: task.id.clone(),
        work_class: work_class(&task.tags),
        complexity: complexity
            .or(task.complexity)
            .map(|value| value.to_string()),
        admitted_at,
        resumed: false,
    }
}
