//! Shared first-failure planner; never reduce mixed jobs to a retry permission.
//!
//! [`plan`] plans and admits the failed-jobs retry, [`send`] sends a reserved
//! retry and records its history, [`classification`] holds the unchanged-head
//! classification and no-edit repair reruns, and [`conclusion`] reads
//! conclusion-only and dependency-handoff failure sets.

mod classification;
mod conclusion;
mod plan;
mod send;

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{CiRerunPlan, CiRerunRoot};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::operations;

pub(super) use classification::request_classification_rerun;
pub(in crate::executor::automation::vcs) use classification::request_no_edit_repair_rerun;
pub(in crate::executor::automation::vcs) use conclusion::handoff_step_set;
pub(in crate::executor::automation::vcs) use plan::plan_ci_rerun;
pub(super) use send::infrastructure_backoff;

pub(in crate::executor::automation::vcs) enum CiRerunStep {
    NotEligible,
    /// A durable infrastructure backoff; it never expires into source repair.
    Backoff(String),
    Wait(String),
    /// [ORB-21586] The retry's evidence could not be read or authenticated,
    /// for this reason. Unlike [`CiRerunStep::Wait`], nothing is in flight:
    /// the landing bounds how often it repeats before it escalates.
    Unreadable(String),
    Acted(Box<CiRerunPlan>),
    /// Admitted reruns are spent. The reason names the stopped jobs or the
    /// exhausted infrastructure classification and count; no repair runs.
    BudgetSpent(String),
}

/// The admission sentence for a head that already used its one CI retry.
const SHARED_RETRY_BUDGET_SPENT: &str = "the shared unchanged-head CI retry budget is spent";

const NO_AUTHENTICATED_PLAN: &str = "CI retry adapter returned no authenticated plan";

/// Whether `error` is the deserialisation failure [`authenticated_plan`]
/// itself produced, as opposed to a failure of the request it sent.
pub(super) fn is_no_authenticated_plan_error(error: &OrbitError) -> bool {
    matches!(error, OrbitError::Execution(message) if message == NO_AUTHENTICATED_PLAN)
}

/// The one `ci.rerun_plan` request: the exact identity a rerun plan must
/// authenticate. [`Self::to_input`] is the only place its keys are named.
pub(super) struct RerunPlanRequest<'a> {
    pub(super) workspace: &'a Path,
    pub(super) pr: String,
    pub(super) head: &'a str,
    pub(super) head_branch: &'a str,
    pub(super) base_branch: &'a str,
    pub(super) accepted_base: &'a str,
    pub(super) run_id: u64,
    pub(super) roots: &'a [CiRerunRoot],
    pub(super) deadline_ms: u64,
}

impl RerunPlanRequest<'_> {
    pub(super) fn to_input(&self) -> Value {
        json!({"workspace_path":self.workspace,"pr":self.pr,"head_sha":self.head,"head_branch":self.head_branch,"base_branch":self.base_branch,"accepted_base":self.accepted_base,"run_id":self.run_id,"roots":self.roots,"deadline_ms":self.deadline_ms})
    }

    /// Whether `plan` is exactly the plan this request asked for.
    pub(super) fn accepts(&self, plan: &CiRerunPlan) -> bool {
        !(plan.schema_version != 1
            || plan.head_sha != self.head
            || plan.pr_number != self.pr
            || plan.head_branch != self.head_branch
            || plan.base_branch != self.base_branch
            || plan.accepted_base != self.accepted_base
            || plan.run_id != self.run_id
            || plan.roots.as_slice() != self.roots)
    }
}

/// The adapter's plan did not match the exact request that was sent.
pub(super) struct PlanMismatch;

pub(super) fn authenticated_plan<H: RuntimeHost + ?Sized>(
    host: &H,
    request: &RerunPlanRequest<'_>,
) -> Result<Result<CiRerunPlan, PlanMismatch>, OrbitError> {
    let response = host.run_private_vcs_operation(operations::CI_RERUN_PLAN, request.to_input())?;
    let plan: CiRerunPlan = serde_json::from_value(response["plan"].clone())
        .map_err(|_| OrbitError::Execution(NO_AUTHENTICATED_PLAN.into()))?;
    Ok(if request.accepts(&plan) {
        Ok(plan)
    } else {
        Err(PlanMismatch)
    })
}
