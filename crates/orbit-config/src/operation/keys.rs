/// When automatic review runs before landing: `off`, `before_landing`
/// (both recommended), or the deprecated `before_pr`.
pub const REVIEW_TIMING_KEY: &str = "review.timing";
/// Actionable deprecation note for an explicit `before_pr` value, naming the
/// deciding key and the recommended replacement. Emitted as a bounded warning;
/// effective behavior stays before-PR.
pub const BEFORE_PR_VALUE_DEPRECATION_NOTE: &str = "review.timing = `before_pr` (or `before-pr`) is \
     deprecated but still holds PR creation for a reviewer; new setups use `off` or \
     `before_landing`, and new run requests use `--review before-landing`. Migrate only with an \
     explicit `orbit config set review.timing before_landing` (add `--global` only for the global layer)";
/// The deprecated before-PR switch, translated to [`REVIEW_TIMING_KEY`].
pub const LEGACY_REVIEW_BEFORE_PR_KEY: &str = "review.before_pr";
/// The deprecated before-landing switch, translated to [`REVIEW_TIMING_KEY`].
pub const LEGACY_REVIEW_BEFORE_LANDING_KEY: &str = "review.before_landing";
/// Machines whose claimed leaves review before landing even while the
/// owner's own `review.before_landing` is off.
pub const REVIEW_BEFORE_LANDING_HOSTS_KEY: &str = "review.before_landing_hosts";
/// The before-PR review's time limit.
pub const REVIEW_MINUTES_KEY: &str = "review.minutes";
/// Whether selective before-landing review observes or enforces.
pub const SELECTIVE_MODE_KEY: &str = "review.selective_before_landing.mode";
/// Probability a candidate's estimated risk must strictly exceed to be
/// selected; unset disables the risk predicate.
pub const SELECTIVE_RISK_THRESHOLD_KEY: &str = "review.selective_before_landing.risk_threshold";
/// Repository-relative path globs that select a candidate; empty disables
/// the path predicate.
pub const SELECTIVE_PATHS_KEY: &str = "review.selective_before_landing.paths";
/// What a configured risk predicate does without a usable estimate.
pub const SELECTIVE_UNKNOWN_RISK_KEY: &str = "review.selective_before_landing.unknown_risk";
/// The retired review timing enum, translated on load.
pub const LEGACY_REVIEW_POLICY_KEY: &str = "operation.review_policy";
/// The retired lineage minutes, translated to [`REVIEW_MINUTES_KEY`].
pub const LEGACY_REVIEW_MINUTES_KEY: &str = "operation.review_minutes";

/// Checks a claimed leaf's host owes for the paths it changed.
pub const REVIEW_HOST_EVIDENCE_KEY: &str = "review.host_evidence";

/// The crew, or crew pool, of automatic review: the after-landing review
/// tasks `delivery-code-review` mints, and the reviewer before landing while
/// [`PRE_LANDING_REVIEW_CREW_KEY`] is unset.
pub const REVIEW_CREW_KEY: &str = "workflow.purpose_crews.review";
/// The crew, or crew pool, of before-PR and before-landing review.
pub const PRE_LANDING_REVIEW_CREW_KEY: &str = "workflow.purpose_crews.pre_landing_review";
/// The deprecated spelling of [`REVIEW_CREW_KEY`].
pub const LEGACY_REVIEW_CREW_KEY: &str = "operation.review_crew";
