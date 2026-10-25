//! Light delivery [ORB-16855]: the `skip-review` and `skip-validation` task
//! tags, and when a candidate keeps them. Mapped review-skip tags
//! (`workflow.review_skip_tags.<tag>`) reuse the same decision with their
//! own path allow-list and an optional base-pinned guard.
//!
//! A tag applies to a candidate only when every task it delivers carries the
//! tag and every path the candidate changes matches the tag's allow-list
//! (and its guard, when set, passes). Otherwise the tag is ignored and the
//! task gets normal review and validation; either way the decision is
//! recorded in the task's history.

use serde::{Deserialize, Serialize};

/// Skip pre-PR, before-landing and after-landing agent review.
pub const SKIP_REVIEW_TAG: &str = "skip-review";

/// Skip local validation: the implementer's workspace gates, required
/// validation and final validation.
pub const SKIP_VALIDATION_TAG: &str = "skip-validation";

/// Both light-delivery tags. Only filing or an operator update may add one.
pub const LIGHT_DELIVERY_TAGS: [&str; 2] = [SKIP_REVIEW_TAG, SKIP_VALIDATION_TAG];

/// The configuration key holding the allow-list.
pub const LIGHT_DELIVERY_PATHS_KEY: &str = "workflow.light_delivery_paths";

/// The configuration key holding the per-tag review-skip table.
pub const REVIEW_SKIP_TAGS_KEY: &str = "workflow.review_skip_tags";

/// Most tags `workflow.review_skip_tags` may map.
pub const MAX_REVIEW_SKIP_TAGS: usize = 32;

/// One mapped review-skip tag's profile: the paths a candidate carrying
/// the tag may change, and an optional guard executable (repository-relative,
/// read from the delivery's base revision) that must exit 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewSkipProfile {
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
}

impl ReviewSkipProfile {
    /// The config key naming this profile's paths, for history notes.
    #[must_use]
    pub fn paths_key(tag: &str) -> String {
        format!("{REVIEW_SKIP_TAGS_KEY}.{tag}.paths")
    }
}

/// How a mapped tag's guard judged a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GuardOutcome {
    /// The profile sets no guard.
    #[default]
    NotConfigured,
    /// The guard is set but was not run (paths or tagging already failed).
    NotRun,
    /// The guard exited 0.
    Passed,
    /// The guard exited non-zero, timed out or could not run; `detail`
    /// names the exit status or cause.
    Failed { detail: String },
    /// The guard file is absent at the delivery's base revision.
    MissingAtBase { guard: String },
}

impl GuardOutcome {
    /// Whether the guard permits the skip.
    #[must_use]
    pub fn permits(&self) -> bool {
        matches!(self, Self::NotConfigured | Self::Passed)
    }

    fn reason(&self) -> Option<String> {
        match self {
            Self::Failed { detail } => Some(format!("its guard failed: {detail}")),
            Self::MissingAtBase { guard } => Some(format!(
                "its guard `{guard}` is missing at the base revision"
            )),
            _ => None,
        }
    }
}

/// The default allow-list: documentation (`docs/**`, Markdown outside
/// `crates/**` and `plugin/**`), CI configuration (`.github/**`) and CI
/// helper scripts. A `!` pattern excludes what it matches.
pub const DEFAULT_LIGHT_DELIVERY_PATHS: &[&str] = &[
    "docs/**",
    "**/*.md",
    "!crates/**",
    "!plugin/**",
    ".github/**",
    "scripts/ci-*",
    "scripts/test-*-workflow*.py",
];

/// Task history event: a light-delivery tag applied to a candidate.
pub const LIGHT_DELIVERY_APPLIED_EVENT: &str = "light_delivery_applied";

/// Task history event: a light-delivery tag was ignored for a candidate, and
/// why.
pub const LIGHT_DELIVERY_IGNORED_EVENT: &str = "light_delivery_ignored";

/// Whether a candidate's changed paths keep it a light delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LightDeliveryJudgement {
    /// Every one of `paths` changed paths matches the allow-list.
    Light { paths: usize },
    /// The candidate changes nothing.
    Empty,
    /// `count` changed paths are outside the allow-list; `first` is one.
    Outside { first: String, count: usize },
}

impl LightDeliveryJudgement {
    /// Judge `changed` paths of which `outside` fall outside the allow-list.
    #[must_use]
    pub fn new(changed: usize, outside: &[&str]) -> Self {
        match outside.first() {
            _ if changed == 0 => Self::Empty,
            None => Self::Light { paths: changed },
            Some(first) => Self::Outside {
                first: (*first).to_string(),
                count: outside.len(),
            },
        }
    }

    /// Whether the candidate is light.
    #[must_use]
    pub fn is_light(&self) -> bool {
        matches!(self, Self::Light { .. })
    }

    fn reason(&self, key: &str) -> String {
        match self {
            Self::Light { paths } => {
                format!("every changed path ({paths}) matches `{key}`")
            }
            Self::Empty => "the candidate changes no path".to_string(),
            Self::Outside { first, count: 1 } => {
                format!("`{first}` is outside `{key}`")
            }
            Self::Outside { first, count } => format!(
                "`{first}` and {} other changed path(s) are outside `{key}`",
                count - 1
            ),
        }
    }
}

/// One light-delivery tag's decision for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightDeliveryDecision {
    /// [`SKIP_REVIEW_TAG`], [`SKIP_VALIDATION_TAG`] or a mapped
    /// `workflow.review_skip_tags` tag (normalized).
    pub tag: String,
    /// The candidate commit judged.
    pub candidate: String,
    pub judgement: LightDeliveryJudgement,
    /// Tasks the candidate delivers that do not carry the tag.
    pub untagged: Vec<String>,
    /// The config key naming the allow-list judged against
    /// (`workflow.light_delivery_paths` for the built-in tags).
    pub profile_key: String,
    /// The mapped tag's guard outcome; `NotConfigured` for built-ins.
    pub guard: GuardOutcome,
}

impl LightDeliveryDecision {
    /// The decision for `tag` over the delivered tasks, given as `(id, tags)`
    /// pairs, or `None` when no task carries the tag and there is nothing to
    /// decide or record.
    #[must_use]
    pub fn decide(
        tag: &str,
        tasks: &[(&str, &[String])],
        candidate: &str,
        judgement: LightDeliveryJudgement,
    ) -> Option<Self> {
        Self::decide_profile(
            tag,
            tasks,
            candidate,
            judgement,
            LIGHT_DELIVERY_PATHS_KEY,
            GuardOutcome::NotConfigured,
        )
    }

    /// [`Self::decide`] for a mapped review-skip tag: `profile_key` names
    /// its `paths`, `guard` its guard's outcome.
    #[must_use]
    pub fn decide_profile(
        tag: &str,
        tasks: &[(&str, &[String])],
        candidate: &str,
        judgement: LightDeliveryJudgement,
        profile_key: &str,
        guard: GuardOutcome,
    ) -> Option<Self> {
        let untagged = tasks
            .iter()
            .filter(|(_, tags)| !tags.iter().any(|carried| carried == tag))
            .map(|(id, _)| (*id).to_string())
            .collect::<Vec<_>>();
        (untagged.len() < tasks.len()).then(|| Self {
            tag: tag.to_string(),
            candidate: candidate.to_string(),
            judgement,
            untagged,
            profile_key: profile_key.to_string(),
            guard,
        })
    }

    /// Whether this decision skips agent review (built-in `skip-review`
    /// or a mapped review-skip tag), as opposed to validation.
    #[must_use]
    pub fn skips_review(&self) -> bool {
        self.tag != SKIP_VALIDATION_TAG
    }

    /// Whether the tag applies: every task carries it and the candidate is
    /// light.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.untagged.is_empty() && self.judgement.is_light() && self.guard.permits()
    }

    /// The task history event this decision records.
    #[must_use]
    pub fn event(&self) -> &'static str {
        if self.applies() {
            LIGHT_DELIVERY_APPLIED_EVENT
        } else {
            LIGHT_DELIVERY_IGNORED_EVENT
        }
    }

    /// The task history note: the tag, the candidate, what happened and why.
    #[must_use]
    pub fn note(&self) -> String {
        let tag = &self.tag;
        let candidate = short(&self.candidate);
        if self.applies() {
            let skipped = if self.skips_review() {
                "no agent review runs before the pull request, before landing or after landing"
            } else {
                "no local validation command runs"
            };
            let guard_note = if matches!(self.guard, GuardOutcome::Passed) {
                " and its guard passed"
            } else {
                ""
            };
            return format!(
                "`{tag}` applied to candidate {candidate}: {}{guard_note}, so {skipped}.",
                self.judgement.reason(&self.profile_key)
            );
        }
        let normal = if self.skips_review() {
            "review"
        } else {
            "validation"
        };
        let reason = if !self.untagged.is_empty() {
            format!(
                "{} in this delivery do(es) not carry it",
                self.untagged.join(", ")
            )
        } else if let Some(reason) = self.guard.reason() {
            reason
        } else {
            self.judgement.reason(&self.profile_key)
        };
        format!("`{tag}` ignored for candidate {candidate}: {reason}; normal {normal} applies.")
    }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
