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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::TaskHistoryEntry;

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
        RecordedLightDeliveryDecision::from_decision(self).note()
    }
}

/// How a recorded light-delivery decision turned out, as its history event
/// and note render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedLightDeliveryOutcome {
    /// The tag applied; `guard_passed` when the note says the guard passed.
    Applied { guard_passed: bool },
    /// The tag was ignored and normal review or validation applies.
    Ignored,
}

/// The one reason a recorded light-delivery note gives, in rendering
/// precedence: untagged tasks, then the guard, then the path judgement.
/// Each variant holds only what the note renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedLightDeliveryReason {
    /// Delivered tasks that do not carry the tag.
    Untagged { tasks: Vec<String> },
    /// The mapped tag's guard failed; `detail` names the exit or cause.
    GuardFailed { detail: String },
    /// The mapped tag's guard is absent at the base revision.
    GuardMissingAtBase { guard: String },
    /// Every one of `paths` changed paths matches `profile_key`.
    Light { paths: usize, profile_key: String },
    /// The candidate changes no path.
    Empty,
    /// `count` changed paths, `first` among them, are outside `profile_key`.
    Outside {
        first: String,
        count: usize,
        profile_key: String,
    },
}

/// The replay identity of a recorded light-delivery decision: exactly the
/// information its history event and note carry, so two decisions share a
/// key exactly when they would record the same event and note. The
/// candidate is the abbreviated commit the note shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightDeliveryReplayKey {
    pub tag: String,
    /// The candidate commit as recorded (its first 12 characters).
    pub candidate: String,
    pub outcome: RecordedLightDeliveryOutcome,
    pub reason: RecordedLightDeliveryReason,
}

impl LightDeliveryReplayKey {
    /// The key `decision` records.
    #[must_use]
    pub fn from_decision(decision: &LightDeliveryDecision) -> Self {
        let applied = decision.applies();
        let judgement_reason = || match &decision.judgement {
            LightDeliveryJudgement::Light { paths } => RecordedLightDeliveryReason::Light {
                paths: *paths,
                profile_key: decision.profile_key.clone(),
            },
            LightDeliveryJudgement::Empty => RecordedLightDeliveryReason::Empty,
            LightDeliveryJudgement::Outside { first, count } => {
                RecordedLightDeliveryReason::Outside {
                    first: first.clone(),
                    count: *count,
                    profile_key: decision.profile_key.clone(),
                }
            }
        };
        let (outcome, reason) = if applied {
            (
                RecordedLightDeliveryOutcome::Applied {
                    guard_passed: matches!(decision.guard, GuardOutcome::Passed),
                },
                judgement_reason(),
            )
        } else {
            let reason = if !decision.untagged.is_empty() {
                RecordedLightDeliveryReason::Untagged {
                    tasks: decision.untagged.clone(),
                }
            } else {
                match &decision.guard {
                    GuardOutcome::Failed { detail } => RecordedLightDeliveryReason::GuardFailed {
                        detail: detail.clone(),
                    },
                    GuardOutcome::MissingAtBase { guard } => {
                        RecordedLightDeliveryReason::GuardMissingAtBase {
                            guard: guard.clone(),
                        }
                    }
                    GuardOutcome::NotConfigured | GuardOutcome::NotRun | GuardOutcome::Passed => {
                        judgement_reason()
                    }
                }
            };
            (RecordedLightDeliveryOutcome::Ignored, reason)
        };
        Self {
            tag: decision.tag.clone(),
            candidate: short(&decision.candidate).to_string(),
            outcome,
            reason,
        }
    }
}

/// A light-delivery decision as task history records it: the one encoder
/// every history writer uses, and the one compatibility codec that reads
/// the recorded event and note back into a [`LightDeliveryReplayKey`]. The
/// persisted bytes are the event name and prose note; nothing else is
/// stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedLightDeliveryDecision {
    key: LightDeliveryReplayKey,
}

const APPLIED_INFIX: &str = "` applied to candidate ";
const IGNORED_INFIX: &str = "` ignored for candidate ";
const GUARD_PASSED_NOTE: &str = " and its guard passed";
const UNTAGGED_SUFFIX: &str = " in this delivery do(es) not carry it";
const GUARD_FAILED_PREFIX: &str = "its guard failed: ";
const GUARD_MISSING_PREFIX: &str = "its guard `";
const GUARD_MISSING_SUFFIX: &str = "` is missing at the base revision";
const EMPTY_REASON: &str = "the candidate changes no path";
const LIGHT_PREFIX: &str = "every changed path (";
const LIGHT_INFIX: &str = ") matches `";
const OUTSIDE_ONE_INFIX: &str = "` is outside `";
const OUTSIDE_MANY_INFIX: &str = " other changed path(s) are outside `";

impl RecordedLightDeliveryDecision {
    /// The record `decision` writes.
    #[must_use]
    pub fn from_decision(decision: &LightDeliveryDecision) -> Self {
        Self {
            key: LightDeliveryReplayKey::from_decision(decision),
        }
    }

    /// The decision `entry` records, or `None` when it is not a
    /// light-delivery event or its note is not one this codec renders
    /// byte for byte. Existing history is read as written, never rewritten.
    #[must_use]
    pub fn decode(entry: &TaskHistoryEntry) -> Option<Self> {
        let applied = match entry.event.as_str() {
            LIGHT_DELIVERY_APPLIED_EVENT => true,
            LIGHT_DELIVERY_IGNORED_EVENT => false,
            _ => return None,
        };
        let note = entry.note.as_deref()?;
        let key = if applied {
            decode_applied(note)?
        } else {
            decode_ignored(note)?
        };
        let record = Self { key };
        // Only a note this codec renders identically decodes, so key
        // equality is exactly the recorded event-and-note equality.
        (record.note() == note).then_some(record)
    }

    /// The replay identity this record carries.
    #[must_use]
    pub fn replay_key(&self) -> &LightDeliveryReplayKey {
        &self.key
    }

    /// The history event this record writes.
    #[must_use]
    pub fn event(&self) -> &'static str {
        match self.key.outcome {
            RecordedLightDeliveryOutcome::Applied { .. } => LIGHT_DELIVERY_APPLIED_EVENT,
            RecordedLightDeliveryOutcome::Ignored => LIGHT_DELIVERY_IGNORED_EVENT,
        }
    }

    /// The history note this record writes: the tag, the abbreviated
    /// candidate, what happened and why.
    #[must_use]
    pub fn note(&self) -> String {
        let LightDeliveryReplayKey {
            tag,
            candidate,
            outcome,
            reason,
        } = &self.key;
        let skips_review = tag != SKIP_VALIDATION_TAG;
        let reason = render_reason(reason);
        match outcome {
            RecordedLightDeliveryOutcome::Applied { guard_passed } => {
                let skipped = applied_skipped(skips_review);
                let guard_note = if *guard_passed { GUARD_PASSED_NOTE } else { "" };
                format!("`{tag}{APPLIED_INFIX}{candidate}: {reason}{guard_note}, so {skipped}.")
            }
            RecordedLightDeliveryOutcome::Ignored => {
                let normal = ignored_normal(skips_review);
                format!("`{tag}{IGNORED_INFIX}{candidate}: {reason}; normal {normal} applies.")
            }
        }
    }

    /// The history entry recording this decision, written by `by` at `at`.
    #[must_use]
    pub fn history_entry(&self, at: DateTime<Utc>, by: impl Into<String>) -> TaskHistoryEntry {
        TaskHistoryEntry::new(at, by, self.event()).with_note(Some(self.note()))
    }
}

fn applied_skipped(skips_review: bool) -> &'static str {
    if skips_review {
        "no agent review runs before the pull request, before landing or after landing"
    } else {
        "no local validation command runs"
    }
}

fn ignored_normal(skips_review: bool) -> &'static str {
    if skips_review { "review" } else { "validation" }
}

fn render_reason(reason: &RecordedLightDeliveryReason) -> String {
    match reason {
        RecordedLightDeliveryReason::Untagged { tasks } => {
            format!("{}{UNTAGGED_SUFFIX}", tasks.join(", "))
        }
        RecordedLightDeliveryReason::GuardFailed { detail } => {
            format!("{GUARD_FAILED_PREFIX}{detail}")
        }
        RecordedLightDeliveryReason::GuardMissingAtBase { guard } => {
            format!("{GUARD_MISSING_PREFIX}{guard}{GUARD_MISSING_SUFFIX}")
        }
        RecordedLightDeliveryReason::Light { paths, profile_key } => {
            format!("{LIGHT_PREFIX}{paths}{LIGHT_INFIX}{profile_key}`")
        }
        RecordedLightDeliveryReason::Empty => EMPTY_REASON.to_string(),
        RecordedLightDeliveryReason::Outside {
            first,
            count: 1,
            profile_key,
        } => format!("`{first}{OUTSIDE_ONE_INFIX}{profile_key}`"),
        RecordedLightDeliveryReason::Outside {
            first,
            count,
            profile_key,
        } => format!(
            "`{first}` and {}{OUTSIDE_MANY_INFIX}{profile_key}`",
            count - 1
        ),
    }
}

/// `tag` and the rest of a note after the `` `{tag}<infix>{candidate}: ``
/// head, with the candidate.
fn decode_head<'a>(note: &'a str, infix: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let (tag, rest) = note.strip_prefix('`')?.split_once(infix)?;
    let (candidate, rest) = rest.split_once(": ")?;
    Some((tag, candidate, rest))
}

fn decode_applied(note: &str) -> Option<LightDeliveryReplayKey> {
    let (tag, candidate, rest) = decode_head(note, APPLIED_INFIX)?;
    let skipped = applied_skipped(tag != SKIP_VALIDATION_TAG);
    let rest = rest
        .strip_suffix('.')?
        .strip_suffix(skipped)?
        .strip_suffix(", so ")?;
    let (rest, guard_passed) = match rest.strip_suffix(GUARD_PASSED_NOTE) {
        Some(rest) => (rest, true),
        None => (rest, false),
    };
    let reason = decode_light(rest)?;
    Some(LightDeliveryReplayKey {
        tag: tag.to_string(),
        candidate: candidate.to_string(),
        outcome: RecordedLightDeliveryOutcome::Applied { guard_passed },
        reason,
    })
}

fn decode_ignored(note: &str) -> Option<LightDeliveryReplayKey> {
    let (tag, candidate, rest) = decode_head(note, IGNORED_INFIX)?;
    let normal = ignored_normal(tag != SKIP_VALIDATION_TAG);
    let reason = rest
        .strip_suffix(" applies.")?
        .strip_suffix(normal)?
        .strip_suffix("; normal ")?;
    let reason = if let Some(tasks) = reason.strip_suffix(UNTAGGED_SUFFIX) {
        RecordedLightDeliveryReason::Untagged {
            tasks: tasks.split(", ").map(str::to_string).collect(),
        }
    } else if let Some(detail) = reason.strip_prefix(GUARD_FAILED_PREFIX) {
        RecordedLightDeliveryReason::GuardFailed {
            detail: detail.to_string(),
        }
    } else if let Some(guard) = reason
        .strip_prefix(GUARD_MISSING_PREFIX)
        .and_then(|rest| rest.strip_suffix(GUARD_MISSING_SUFFIX))
    {
        RecordedLightDeliveryReason::GuardMissingAtBase {
            guard: guard.to_string(),
        }
    } else if reason == EMPTY_REASON {
        RecordedLightDeliveryReason::Empty
    } else if reason.starts_with(LIGHT_PREFIX) {
        decode_light(reason)?
    } else {
        decode_outside(reason)?
    };
    Some(LightDeliveryReplayKey {
        tag: tag.to_string(),
        candidate: candidate.to_string(),
        outcome: RecordedLightDeliveryOutcome::Ignored,
        reason,
    })
}

fn decode_light(reason: &str) -> Option<RecordedLightDeliveryReason> {
    let (paths, profile_key) = reason
        .strip_prefix(LIGHT_PREFIX)?
        .strip_suffix('`')?
        .split_once(LIGHT_INFIX)?;
    Some(RecordedLightDeliveryReason::Light {
        paths: paths.parse().ok()?,
        profile_key: profile_key.to_string(),
    })
}

fn decode_outside(reason: &str) -> Option<RecordedLightDeliveryReason> {
    let body = reason.strip_prefix('`')?.strip_suffix('`')?;
    if let Some((first, profile_key)) = body.split_once(OUTSIDE_ONE_INFIX) {
        return Some(RecordedLightDeliveryReason::Outside {
            first: first.to_string(),
            count: 1,
            profile_key: profile_key.to_string(),
        });
    }
    let (head, profile_key) = body.split_once(OUTSIDE_MANY_INFIX)?;
    let (first, others) = head.rsplit_once("` and ")?;
    let others: usize = others.parse().ok()?;
    Some(RecordedLightDeliveryReason::Outside {
        first: first.to_string(),
        count: others.checked_add(1)?,
        profile_key: profile_key.to_string(),
    })
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
