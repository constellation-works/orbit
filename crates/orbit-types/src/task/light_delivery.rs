//! Light delivery [ORB-16855]: the `skip-review` and `skip-validation` task
//! tags, and when a candidate keeps them.
//!
//! A tag applies to a candidate only when every task it delivers carries the
//! tag and every path the candidate changes matches
//! `workflow.light_delivery_paths`. Otherwise the tag is ignored and the task
//! gets normal review and validation; either way the decision is recorded in
//! the task's history.

/// Skip pre-PR, before-landing and after-landing agent review.
pub const SKIP_REVIEW_TAG: &str = "skip-review";

/// Skip local validation: the implementer's workspace gates, required
/// validation and final validation.
pub const SKIP_VALIDATION_TAG: &str = "skip-validation";

/// Both light-delivery tags. Only filing or an operator update may add one.
pub const LIGHT_DELIVERY_TAGS: [&str; 2] = [SKIP_REVIEW_TAG, SKIP_VALIDATION_TAG];

/// The configuration key holding the allow-list.
pub const LIGHT_DELIVERY_PATHS_KEY: &str = "workflow.light_delivery_paths";

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

    fn reason(&self) -> String {
        match self {
            Self::Light { paths } => {
                format!("every changed path ({paths}) matches `{LIGHT_DELIVERY_PATHS_KEY}`")
            }
            Self::Empty => "the candidate changes no path".to_string(),
            Self::Outside { first, count: 1 } => {
                format!("`{first}` is outside `{LIGHT_DELIVERY_PATHS_KEY}`")
            }
            Self::Outside { first, count } => format!(
                "`{first}` and {} other changed path(s) are outside `{LIGHT_DELIVERY_PATHS_KEY}`",
                count - 1
            ),
        }
    }
}

/// One light-delivery tag's decision for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightDeliveryDecision {
    /// [`SKIP_REVIEW_TAG`] or [`SKIP_VALIDATION_TAG`].
    pub tag: &'static str,
    /// The candidate commit judged.
    pub candidate: String,
    pub judgement: LightDeliveryJudgement,
    /// Tasks the candidate delivers that do not carry the tag.
    pub untagged: Vec<String>,
}

impl LightDeliveryDecision {
    /// The decision for `tag` over the delivered tasks, given as `(id, tags)`
    /// pairs, or `None` when no task carries the tag and there is nothing to
    /// decide or record.
    #[must_use]
    pub fn decide(
        tag: &'static str,
        tasks: &[(&str, &[String])],
        candidate: &str,
        judgement: LightDeliveryJudgement,
    ) -> Option<Self> {
        let untagged = tasks
            .iter()
            .filter(|(_, tags)| !tags.iter().any(|carried| carried == tag))
            .map(|(id, _)| (*id).to_string())
            .collect::<Vec<_>>();
        (untagged.len() < tasks.len()).then(|| Self {
            tag,
            candidate: candidate.to_string(),
            judgement,
            untagged,
        })
    }

    /// Whether the tag applies: every task carries it and the candidate is
    /// light.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.untagged.is_empty() && self.judgement.is_light()
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
        let tag = self.tag;
        let candidate = short(&self.candidate);
        if self.applies() {
            let skipped = if tag == SKIP_REVIEW_TAG {
                "no agent review runs before the pull request, before landing or after landing"
            } else {
                "no local validation command runs"
            };
            return format!(
                "`{tag}` applied to candidate {candidate}: {}, so {skipped}.",
                self.judgement.reason()
            );
        }
        let normal = if tag == SKIP_REVIEW_TAG {
            "review"
        } else {
            "validation"
        };
        let reason = if self.untagged.is_empty() {
            self.judgement.reason()
        } else {
            format!(
                "{} in this delivery do(es) not carry it",
                self.untagged.join(", ")
            )
        };
        format!("`{tag}` ignored for candidate {candidate}: {reason}; normal {normal} applies.")
    }
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
