//! Typed review preferences [ORB-11333] [ORB-13992].
//!
//! Review timing is one key, `review.timing`: `off` (default, recommended),
//! `before_landing` (recommended when review before landing is wanted), or the
//! deprecated `before_pr` (hold PR creation for a reviewer) [ORB-14849].
//! There is one review layer before landing, so the two timings are values of
//! one key rather than two switches. Both are bounded by `review.minutes` per
//! candidate, and `workflow.purpose_crews.review` names the crew of automatic
//! review: one crew, or a pool written like the complexity pools that each
//! review draws one crew from [ORB-15195].
//!
//! `before_pr` (and its `before-pr` alias) is deprecated but fully honoured:
//! an explicit `before_pr` keeps before-PR semantics until an operator
//! intentionally migrates it with `orbit config set review.timing
//! before_landing`. New setups use `off` or `before_landing`, and new run
//! requests use `--review before-landing`.
//! `workflow.purpose_crews.pre_landing_review`, written the same way, names
//! the before-PR and before-landing reviewer on its own and falls back to
//! `workflow.purpose_crews.review` while unset, so the after-landing review
//! tasks `delivery-code-review` mints can keep a different pool.
//! `review.before_landing_hosts` lets an owner whose own deliveries do not
//! review before landing still capture before-landing review for the claims
//! of the machines it lists [ORB-15192]; it is refused beside
//! `review.timing = "before_pr"` for the same reason. After-landing review has
//! no key here at all: its only switch is the `delivery-code-review`
//! auto-task's own `enabled` flag, which Core reads.
//!
//! Each layer states any subset; a value resolves built-in → global →
//! workspace with its winning layer recorded.
//!
//! `[[review.host_evidence]]` declares the checks a claimed leaf's host owes
//! for the paths its candidate changed ([`orbit_types::workflow::HostEvidenceRule`]); a layer that
//! states the list replaces every rule an earlier layer stated.
//!
//! Deprecated spellings are translated when a document is read
//! ([`crate::aliases`]) and warned as deprecated
//! (`registry::DEPRECATED_CONFIG_KEYS`): the `review.before_pr` and
//! `review.before_landing` booleans and the retired `operation.review_policy`
//! enum become `review.timing`, `operation.review_minutes` becomes
//! `review.minutes`, and `operation.review_crew` becomes
//! `workflow.purpose_crews.review`. A legacy `after-landing` policy also
//! enables the auto-task until its flag is set explicitly. The resolved
//! [`OperationPolicy`] keeps its two timing booleans, which run records and
//! claim contracts capture. The retired budget keys
//! (`operation.review_reviewer_starts`, `operation.review_repair_cycles`) are
//! warned and ignored (`registry::REMOVED_CONFIG_KEYS`).

mod crew;
mod hosts;
mod keys;
mod layer;
mod legacy;
mod minutes;
mod policy;
mod selective;
mod timing;

pub use crew::{ReviewCrewPool, ReviewCrewSetting};
pub(crate) use crew::{pre_landing_review_crew, review_crew};
pub(crate) use hosts::before_landing_hosts;
pub use keys::{
    BEFORE_PR_VALUE_DEPRECATION_NOTE, LEGACY_REVIEW_BEFORE_LANDING_KEY,
    LEGACY_REVIEW_BEFORE_PR_KEY, LEGACY_REVIEW_CREW_KEY, PRE_LANDING_REVIEW_CREW_KEY,
    REVIEW_CREW_KEY, REVIEW_HOST_EVIDENCE_KEY, REVIEW_MINUTES_KEY, REVIEW_TIMING_KEY,
    SELECTIVE_MODE_KEY, SELECTIVE_PATHS_KEY, SELECTIVE_RISK_THRESHOLD_KEY,
    SELECTIVE_UNKNOWN_RISK_KEY,
};
pub use layer::{OperationField, OperationLayer, OperationLayerSource};
pub(crate) use legacy::legacy_policy_before_pr;
pub(crate) use minutes::{review_minutes, translate_legacy_review_minutes};
pub use policy::{OPERATION_POLICY_VERSION, OperationPolicy, SelectiveResolved};
pub use selective::{SelectiveLayer, selective_mode_parse, selective_unknown_risk_parse};
pub(crate) use selective::{selective_paths, selective_threshold_value};
pub use timing::ReviewTimingSetting;
