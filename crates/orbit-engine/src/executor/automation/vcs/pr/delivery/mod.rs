//! Fresh delivery evidence for completion-authorized PR runs [ORB-11982].
//!
//! F2026-09-102 records a bundle reaching `done` while its pull request was
//! still open after a base-modification race. The recorded task history shows
//! that `review -> done` transition carried no actor and no authorization note,
//! so the completion activity — which always stamps one — did not write it. The
//! surface that did is not recoverable from the retained evidence. This module
//! therefore hardens what *is* in scope: the automatic path must never be the
//! way an open or foreign pull request becomes `done`.
//!
//! The rule is that completion is permitted by evidence read at completion
//! time, about the exact candidate this run published. A run carries the same
//! `head`, `base`, and `published_head_sha` checkpoints [ORB-11488] already
//! pins for conflict repair; [`DeliveryPin`] turns them into an identity the
//! provider's answer has to match, both before a merge is requested and again
//! before the guarded transition runs. A run without those checkpoints — a
//! resumed or hand-built completion input — enforces only what it carries, so
//! the pins narrow the authorized set and never widen it.

mod pin;
mod state;

pub(in crate::executor::automation::vcs) use super::merge_state::{
    PrMergeState, classify_pr_state,
};
pub(in crate::executor::automation::vcs) use pin::{DeliveryEvidence, DeliveryPin};
pub(in crate::executor::automation::vcs) use state::{
    BlockedCheckWait, OwedHostedChecks, UnreportedChecks, checked_pr_state, owed_hosted_checks,
};
