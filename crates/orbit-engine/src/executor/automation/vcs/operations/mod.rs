mod dispatch;
mod input;
mod pr;
mod process;
mod push_retry;

#[cfg(all(test, unix))]
mod tests;

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

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const SLOW_TIMEOUT_MS: u64 = 30_000;
const LONG_TIMEOUT_MS: u64 = 60_000;

/// The `refusal` a reviewed merge reports, instead of failing, when its
/// caller opts in with `report_base_modified`.
pub(crate) const BASE_MODIFIED_REFUSAL: &str = "base_modified";
