mod check_annotations;
mod ci;
mod dispatch;
mod emission;
mod input;
mod pr;
mod prerequisite;
pub(super) mod process;
mod push_retry;
mod workflow_dependency;
mod workflow_read;

pub(crate) use dispatch::run;
pub(super) use process::{
    GITHUB_TRANSIENT_ATTEMPTS, GITHUB_TRANSIENT_RETRY_DELAY, is_transient_github_failure,
};
pub(crate) use push_retry::valid_candidate_ref;

pub(crate) const PUSH: &str = "push";
/// [ORB-14338] Push one commit to a run-owned `refs/orbit/candidates/` ref.
pub(crate) const CANDIDATE_REF_PUSH: &str = "push.candidate_ref";
/// The namespace a carried candidate ref lives under on `origin`.
pub(crate) const CANDIDATE_REF_PREFIX: &str = "refs/orbit/candidates/";
pub(crate) const PR_LIST: &str = "pr.list";
pub(crate) const PR_CREATE: &str = "pr.create";
pub(crate) const PR_VIEW: &str = "pr.view";
pub(crate) const PR_MERGE: &str = "pr.merge";
pub(crate) const PR_MERGE_CAPABILITIES: &str = "pr.merge_capabilities";
pub(crate) const PR_STATUS: &str = "pr.status";
/// [ORB-15308] Close a pull request a superseding run replaced.
pub(crate) const PR_CLOSE: &str = "pr.close";
/// Which of a pull request's head checks branch protection requires.
pub(crate) const PR_REQUIRED_CHECKS: &str = "pr.required_checks";
/// [ORB-15956] Merge the base's current tip into a pull request's branch.
pub(crate) const PR_UPDATE_BRANCH: &str = "pr.update_branch";
/// [ORB-15956] The hosted CI jobs that ran on one commit.
pub(crate) const CI_COMMIT_JOBS: &str = "ci.commit_jobs";
/// [ORB-15956] One failed job's bounded failed-step log.
pub(crate) const CI_JOB_LOG: &str = "ci.job_log";
/// [ORB-15956] Rerun one workflow run's failed jobs.
pub(crate) const CI_RERUN_FAILED: &str = "ci.rerun_failed";
/// [ORB-16185] One required non-workflow check run's failure annotations.
pub(crate) const CI_CHECK_ANNOTATIONS: &str = "ci.check_annotations";

pub(super) const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const SLOW_TIMEOUT_MS: u64 = 30_000;
const LONG_TIMEOUT_MS: u64 = 60_000;

/// The `refusal` a reviewed merge reports, instead of failing, when its
/// caller opts in with `report_base_modified`.
pub(crate) const BASE_MODIFIED_REFUSAL: &str = "base_modified";

pub(crate) const CI_PREREQUISITE_EVIDENCE: &str = "ci.prerequisite_evidence";
pub(crate) const CI_PREREQUISITE_OBSERVE: &str = "ci.prerequisite_observe";

/// Trusted old-head/current-base workflow emission proof and observation.
pub(crate) const CI_EMISSION_EVIDENCE: &str = "ci.emission_evidence";
pub(crate) const CI_EMISSION_OBSERVE: &str = "ci.emission_observe";

/// Current exact policy gates on an adopted provider head.
pub(crate) const CI_CURRENT_GATES: &str = "ci.current_gates";
