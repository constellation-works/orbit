//! Structural classification of the activities a [`JobV2`] can dispatch.
//!
//! A job declares two structurally distinct kinds of activity:
//!
//! * **step activities** — the work the job exists to do;
//! * **recovery activities** — named by `recovery_activity` at the job level
//!   or on an individual step, dispatched only after that step fails.
//!
//! # Matching what the store actually records
//!
//! The identifier an `invocations` row carries in `activity_id` is *not*
//! uniformly the catalog activity name. The job executor persists a dispatched
//! step's trace under the **step id**, while a recovery dispatch — which has
//! no step of its own — is persisted under the **recovery activity name**.
//! [`JobActivityRoles::step`] therefore holds both identifiers a step
//! invocation can be recorded under: the step's `id`, and the catalog name
//! from its `target` (an inlined [`TargetStep`](super::job_v2::TargetStep)'s `activity_name`, or an
//! unresolved [`TargetRef`](super::job_v2::TargetRef) of the form `activity:<name>`). Collecting only
//! one of the two would leave real step work unattributed and silently deflate
//! any denominator built from this set.
//!
//! Reliability reporting needs the recovery set to be *discovered* rather than
//! enumerated in source: which activity performs recovery is a property of the
//! workspace's job definitions, not of Orbit. [`JobV2::activity_roles`] walks
//! the declared job structure and returns both sets, so a caller can attribute
//! observed invocations without ever hardcoding an activity id [ORB-10588].
//!
//! An activity may legitimately appear in both sets (a job can name the same
//! activity as a step target and as another step's recovery hook). Callers
//! that need disjoint sets should treat `recovery` as taking precedence, since
//! an invocation of a dual-role activity cannot be attributed structurally.

use std::collections::BTreeSet;

use super::ACTIVITY_REF_PREFIX;
use super::job_v2::{JobV2, JobV2Step, JobV2StepBody};

/// The activity ids a job can dispatch, split by the structural role the job
/// definition assigns them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobActivityRoles {
    /// Identifiers a step invocation can be recorded under, at any nesting
    /// depth: each step's `id` plus the catalog name of its target.
    pub step: BTreeSet<String>,
    /// Activities named by `recovery_activity`, at the job level or on a step.
    pub recovery: BTreeSet<String>,
}

impl JobActivityRoles {
    /// Folds another job's roles into this one, for catalog-wide discovery.
    pub fn merge(&mut self, other: &JobActivityRoles) {
        self.step.extend(other.step.iter().cloned());
        self.recovery.extend(other.recovery.iter().cloned());
    }

    /// Activities that are only ever reached as a recovery hook.
    ///
    /// An activity that is also a step target is excluded: its invocations
    /// cannot be attributed to recovery from the job structure alone.
    pub fn recovery_only(&self) -> BTreeSet<String> {
        self.recovery.difference(&self.step).cloned().collect()
    }

    /// Activities that are only ever reached as a step target.
    pub fn step_only(&self) -> BTreeSet<String> {
        self.step.difference(&self.recovery).cloned().collect()
    }
}

impl JobV2 {
    /// Walks this job's declared structure and returns the activity ids it can
    /// dispatch, split into step targets and recovery hooks.
    pub fn activity_roles(&self) -> JobActivityRoles {
        let mut roles = JobActivityRoles::default();
        insert_activity(&mut roles.recovery, self.recovery_activity.as_deref());
        for step in &self.steps {
            collect_step_roles(step, &mut roles);
        }
        roles
    }
}

fn collect_step_roles(step: &JobV2Step, roles: &mut JobActivityRoles) {
    insert_activity(&mut roles.recovery, step.recovery_activity.as_deref());
    // The step id is what the job executor records for a dispatched step, so
    // it belongs in the step set alongside the target's catalog name.
    insert_activity(&mut roles.step, Some(step.id.as_str()));
    match &step.body {
        JobV2StepBody::Target(target) => {
            insert_activity(&mut roles.step, target.activity_name.as_deref());
        }
        JobV2StepBody::TargetRef(reference) => {
            insert_activity(&mut roles.step, Some(reference.target.as_str()));
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_step_roles(branch, roles);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => collect_step_roles(&fan_out.worker, roles),
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_step_roles(nested, roles);
            }
        }
    }
}

/// Normalizes a declared reference to the bare activity id recorded on an
/// invocation row. `target:` references carry the `activity:` namespace
/// prefix; `recovery_activity` and a resolved `activity_name` do not.
fn insert_activity(set: &mut BTreeSet<String>, raw: Option<&str>) {
    let Some(raw) = raw else {
        return;
    };
    let name = raw.strip_prefix(ACTIVITY_REF_PREFIX).unwrap_or(raw).trim();
    if !name.is_empty() {
        set.insert(name.to_string());
    }
}
