//! Provider pull-request answers, parsed once, plus the merge-state classifier,
//! delivery pin and bounded diagnostics those answers feed.
//!
//! Private operations below this module produce the JSON. This module is the
//! one reader of that JSON, so it names no PR, check, landing, base, push or
//! claim module above it. `pr` re-exports the names so older paths keep
//! compiling; retire those shims once callers import `pr_answers` directly.

pub(in crate::executor::automation::vcs) mod answers;
pub(in crate::executor::automation::vcs) mod diagnostics;
pub(in crate::executor::automation::vcs) mod merge_state;
mod pin;

pub use answers::{CheckState, PullRequestStatus};
pub(in crate::executor::automation::vcs) use answers::{
    RequiredChecks, ReviewDecision, read_pr_status, read_pr_status_before,
    read_required_checks_observed,
};
pub(in crate::executor::automation::vcs) use merge_state::{PrMergeState, classify_pr_state};
pub(in crate::executor::automation::vcs) use pin::{DeliveryEvidence, DeliveryPin};
