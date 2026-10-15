use orbit_common::OrbitError;
use serde_json::Value;

use super::ci::{ci_commit_jobs, ci_job_log, ci_rerun_failed};
use super::pr::{
    pr_close, pr_create, pr_list, pr_merge, pr_merge_capabilities, pr_required_checks, pr_status,
    pr_update_branch, pr_view,
};
use super::push_retry::{push, push_candidate_ref};
use super::{
    CANDIDATE_REF_PUSH, CI_COMMIT_JOBS, CI_JOB_LOG, CI_RERUN_FAILED, PR_CLOSE, PR_CREATE, PR_LIST,
    PR_MERGE, PR_MERGE_CAPABILITIES, PR_REQUIRED_CHECKS, PR_STATUS, PR_UPDATE_BRANCH, PR_VIEW,
    PUSH,
};
use super::{CI_PREREQUISITE_EVIDENCE, CI_PREREQUISITE_OBSERVE};

/// Execute the VCS operations owned by deterministic shipment automation.
///
/// This boundary is deliberately separate from `ToolRegistry`: the operation
/// labels are engine-private, are never advertised to agents, and do not pass
/// through public tool authorization or activity allowlists.
pub(crate) fn run(operation: &str, input: &Value) -> Result<Value, OrbitError> {
    match operation {
        super::CI_CURRENT_GATES => super::emission::current_gates(input),
        super::CI_EMISSION_EVIDENCE => super::emission::evidence(input),
        super::CI_EMISSION_OBSERVE => super::emission::observe(input),
        PUSH => push(input),
        CANDIDATE_REF_PUSH => push_candidate_ref(input),
        PR_LIST => pr_list(input),
        PR_CREATE => pr_create(input),
        PR_VIEW => pr_view(input),
        PR_MERGE => pr_merge(input),
        PR_MERGE_CAPABILITIES => pr_merge_capabilities(input),
        PR_STATUS => pr_status(input),
        PR_REQUIRED_CHECKS => pr_required_checks(input),
        PR_CLOSE => pr_close(input),
        PR_UPDATE_BRANCH => pr_update_branch(input),
        CI_PREREQUISITE_EVIDENCE => super::prerequisite::evidence(input),
        CI_PREREQUISITE_OBSERVE => super::prerequisite::observe(input),
        CI_COMMIT_JOBS => ci_commit_jobs(input),
        CI_JOB_LOG => ci_job_log(input),
        CI_RERUN_FAILED => ci_rerun_failed(input),
        other => Err(OrbitError::InvalidInput(format!(
            "unknown private automation VCS operation '{other}'"
        ))),
    }
}
