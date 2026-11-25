use orbit_common::OrbitError;
use serde_json::Value;

use super::ci::{ci_commit_jobs, ci_job_log, ci_rerun_failed};
use super::pr::{
    pr_close, pr_create, pr_list, pr_list_open, pr_merge, pr_merge_capabilities,
    pr_required_checks, pr_status, pr_update_branch, pr_view,
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
pub(crate) fn run(
    process_env: &orbit_common::Env,
    operation: &str,
    input: &Value,
) -> Result<Value, OrbitError> {
    match operation {
        super::CI_RERUN_PLAN => super::ci_rerun::ci_rerun_plan(process_env, input),
        super::CI_CURRENT_GATES => super::emission::current_gates(process_env, input),
        super::CI_EMISSION_EVIDENCE => super::emission::evidence(process_env, input),
        super::CI_EMISSION_OBSERVE => super::emission::observe(process_env, input),
        PUSH => push(process_env, input),
        CANDIDATE_REF_PUSH => push_candidate_ref(process_env, input),
        super::EVIDENCE_REF_PUSH => super::evidence::push_evidence_ref(process_env, input),
        PR_LIST => pr_list(process_env, input),
        super::PR_LIST_OPEN => pr_list_open(process_env, input),
        PR_CREATE => pr_create(process_env, input),
        super::PR_ADD_LABELS => super::pr::add_labels(process_env, input),
        PR_VIEW => pr_view(process_env, input),
        super::PR_EDIT_BODY => super::pr::edit_pr_body(process_env, input),
        super::PR_READY => super::pr::mark_pr_ready(process_env, input),
        PR_MERGE => pr_merge(process_env, input),
        PR_MERGE_CAPABILITIES => pr_merge_capabilities(process_env, input),
        PR_STATUS => pr_status(process_env, input),
        PR_REQUIRED_CHECKS => pr_required_checks(process_env, input),
        PR_CLOSE => pr_close(process_env, input),
        PR_UPDATE_BRANCH => pr_update_branch(process_env, input),
        super::PR_RETRIGGER_CHECKS => super::pr::pr_retrigger_checks(process_env, input),
        CI_PREREQUISITE_EVIDENCE => super::prerequisite::evidence(process_env, input),
        CI_PREREQUISITE_OBSERVE => super::prerequisite::observe(process_env, input),
        CI_COMMIT_JOBS => ci_commit_jobs(process_env, input),
        CI_JOB_LOG => serde_json::to_value(ci_job_log(process_env, input)?)
            .map_err(|error| OrbitError::Execution(error.to_string())),
        CI_RERUN_FAILED => ci_rerun_failed(process_env, input),
        super::CI_CHECK_ANNOTATIONS => {
            super::check_annotations::ci_check_annotations(process_env, input)
        }
        other => Err(OrbitError::InvalidInput(format!(
            "unknown private automation VCS operation '{other}'"
        ))),
    }
}
