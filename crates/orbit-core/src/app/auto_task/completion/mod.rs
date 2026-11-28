//! Completion contracts of auto-task job steps
//! (`docs/design/activity-job/specs/auto-task-step.md` §4): resolve a step's
//! declared contract, judge a completed task's evidence against the owner's
//! records and a granted workspace adapter, and keep the accepted-result
//! history.
//!
//! A receipt's task, run, digest, source and time come from durable owner
//! records. The evidence a run leaves is compared with them, and an adapter's
//! verdict only says whether the evidence meets the workspace's policy: neither
//! is a certificate, and an agent's final message is never read. What a form
//! requires lives in the workspace adapter, so nothing here knows a form.
//!
//! Every failure holds the step. A hold is rechecked on later passes until the
//! step's expiry, so a finding resolved in time can clear it, while a hold still
//! standing at expiry times out.

mod adapter;
mod binding;
mod contract;
mod history;
mod trust;

#[cfg(feature = "test-support")]
pub(crate) mod test_support;

pub(super) use binding::{bind_completion_result, evidence_binding_note};
pub(super) use contract::{ProofDepth, contract_admission_hold, prior_proof_hold, reused_result};
pub(super) use history::{accepted_history, record_accepted_result};
