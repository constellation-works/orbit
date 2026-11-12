//! Failure-activity and final-recovery checkpoints.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::TaskStatus;
use crate::workflow::final_recovery::{FaultAttribution, FinalRecoveryDecision};

/// Durable result of a job-level terminal failure activity.
///
/// A failure activity is not a successful workflow step, so its output cannot
/// live in `step_outputs`. Keeping it separately preserves the evidence needed
/// to resume from a recovery action without treating the failed step as
/// completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureActivityCheckpoint {
    pub activity_name: String,
    pub failed_step_id: String,
    pub output: Value,
}

/// One weighted member of the pool an activity crew draw ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityCrewPoolMember {
    pub name: String,
    /// Configured weight before usage-pressure scaling.
    pub weight: u32,
    /// Raw weekly usage pressure; absent on static and older draws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<f64>,
    /// Weight after pressure scaling and ticket rounding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_weight: Option<f64>,
}

/// A crew an activity drew from a configured pool, frozen for the run.
///
/// An activity whose crew comes from a weighted pool (its `crew_config_key`)
/// draws once; every later dispatch of that key in the run, and every resume
/// seeded from it, reuses this record instead of rerolling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityCrewDraw {
    /// Canonical crew name the draw selected.
    pub crew: String,
    /// The configuration the pool came from (`workflow.purpose_crews.final_recovery`,
    /// `workflow.<tier>_complexity_crews`), plus a note when the draw fell
    /// back to `workflow.default_crew` or met a provider limit. Stored as
    /// that exact string; its origin is classified once when it is read.
    pub source: ActivityDrawSource,
    /// The permitted members and weights the draw ran on, so `run show`
    /// can explain the choice.
    pub eligible_pool: Vec<ActivityCrewPoolMember>,
    /// The effort a `<crew>@auto` member resolved to, and why; absent when
    /// the drawn member names its effort (or none) itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_effort: Option<crate::identity::AutoEffortChoice>,
}

/// Where an activity crew draw's pool came from, classified once from its
/// recorded source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityDrawOrigin {
    /// The draw fell back to `workflow.default_crew`.
    DefaultCrew,
    /// The draw ran on the configured pool `key` (a `workflow.*` crew pool
    /// key such as `workflow.purpose_crews.task_pilot` or
    /// `workflow.low_complexity_crews`).
    ConfiguredPool { key: String },
    /// A recorded source in no known form; kept verbatim.
    Other,
}

/// The provenance an [`ActivityCrewDraw`] records: the exact descriptive
/// string, notes included, and the origin classified from it.
///
/// Serializes as that string, byte for byte, so persisted run states keep
/// their shape; reading one classifies it with the same decoder new draws
/// use. A string in no known form reads as [`ActivityDrawOrigin::Other`] and
/// writes back unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityDrawSource {
    text: String,
    origin: ActivityDrawOrigin,
}

impl ActivityDrawSource {
    /// Configuration key every default-crew fallback source begins with.
    const DEFAULT_CREW_KEY: &'static str = "workflow.default_crew";

    /// Classify a descriptive draw source: the one decoder for new draws and
    /// persisted ones. The origin is the leading configuration key, the text
    /// before any ` `, `;` or `(` note.
    pub fn parse(text: impl Into<String>) -> Self {
        let text = text.into();
        let origin = if text.starts_with(Self::DEFAULT_CREW_KEY) {
            ActivityDrawOrigin::DefaultCrew
        } else {
            let key = text.split([' ', ';', '(']).next().unwrap_or_default();
            if key.len() > "workflow.".len() && key.starts_with("workflow.") {
                ActivityDrawOrigin::ConfiguredPool {
                    key: key.to_string(),
                }
            } else {
                ActivityDrawOrigin::Other
            }
        };
        Self { text, origin }
    }

    /// This source with `note` appended as `<source>; <note>`, keeping its
    /// origin.
    #[must_use]
    pub fn with_note(&self, note: &str) -> Self {
        Self {
            text: format!("{}; {note}", self.text),
            origin: self.origin.clone(),
        }
    }

    /// Where the drawn pool came from.
    pub fn origin(&self) -> &ActivityDrawOrigin {
        &self.origin
    }

    /// The exact recorded string, notes included.
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for ActivityDrawSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl From<String> for ActivityDrawSource {
    fn from(text: String) -> Self {
        Self::parse(text)
    }
}

impl From<&str> for ActivityDrawSource {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

impl Serialize for ActivityDrawSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for ActivityDrawSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::parse)
    }
}

/// The task revision final recovery observed when it was admitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryObservedTask {
    pub status: TaskStatus,
    pub updated_at: DateTime<Utc>,
    /// Digest of the task's lifecycle content; absent on a checkpoint
    /// written before it was recorded, which compares by `updated_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_digest: Option<String>,
}

/// Idempotency key of a run's final-recovery decision: the run that admitted
/// it and that run's attempt. The applier names the run in the task comment
/// it writes, and a decision whose run already has that comment is not
/// applied again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryKey {
    pub run_id: String,
    pub attempt: u32,
}

/// Exact HEAD advance observed by the engine around final recovery dispatch.
/// It authorizes only this worktree's recorded repair, never later HEAD drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryRepairCommit {
    pub workspace_path: std::path::PathBuf,
    pub head_sha_before: String,
    pub head_sha: String,
}

/// Instructions and refused output delivered to the step final recovery resumes.
/// Stored before stale step checkpoints are removed, so a process restart keeps
/// the same repair mandate. Older checkpoints omit this context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryResume {
    /// Top-level resume target, including all targets nested in that step.
    pub step_id: String,
    /// Top-level step whose failure caused recovery.
    pub failed_step_id: String,
    /// Recovery's explanation of what the resumed step must address.
    pub rationale: String,
    /// Original boundary refusal, before the rerun replaces it. Bounded like
    /// final recovery's own `error_message`: an oversized one keeps its head
    /// and tail around a truncation marker.
    pub refusal: String,
    /// Original failed step output; null when the step produced none. Bounded
    /// like final recovery's `step_outputs`.
    pub refused_output: serde_json::Value,
}

/// A run's job-level final recovery [ORB-13907], recorded when it is admitted.
///
/// Admission writes this before the activity is dispatched, so a crash during
/// the activity, a resume decision, or an operator resume seeded from this
/// state never dispatches final recovery for the run a second time. The
/// decision is recorded here before it touches the task. A run resumed from
/// state that holds a decision other than `resume` applies that decision again
/// instead of dispatching, so a crash at any point converges on one outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryCheckpoint {
    pub key: FinalRecoveryKey,
    /// Top-level step whose failure admitted final recovery.
    pub failed_step_id: String,
    pub task_id: String,
    /// The task as it stood at admission; the applier refuses a decision once
    /// the task has changed since. Absent for a claimed leaf, whose task lives
    /// on its owner and is re-read there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<FinalRecoveryObservedTask>,
    /// Base ref a `complete_no_diff` commit must be reachable from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    pub admitted_at: DateTime<Utc>,
    /// The decision acted on, recorded before it is applied. The engine's
    /// substitutions (an invalid resume step, a failed activity) are recorded
    /// as the `escalate` they became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<FinalRecoveryDecision>,
    /// Whose fault the failure was, recorded with the decision. A checkpoint
    /// written before faults were recorded reads as `unknown`.
    #[serde(default)]
    pub fault: FaultAttribution,
    /// A repair committed during recovery, recorded with the resume decision.
    /// Older run states carry no permission to accept a moved HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_commit: Option<FinalRecoveryRepairCommit>,
    /// Repair context captured before the resumed steps lose their outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_context: Option<FinalRecoveryResume>,
    /// What applying the decision did; absent while the decision is only
    /// intended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<FinalRecoveryCheckpointOutcome>,
}

/// What applying a final-recovery decision did, recorded on its checkpoint.
///
/// Persisted run states written before this type existed hold the outcome as
/// the string `resume`, `settled: <detail>` or `escalated: <detail>`. Those
/// legacy strings deserialize into this enum (the `settled:`/`escalated:`
/// prefix is split once, here and nowhere else); a string in none of those
/// forms is a deserialization error. New writes use the tagged form.
/// [`fmt::Display`] reproduces the legacy text, so `run show` output is
/// unchanged.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FinalRecoveryCheckpointOutcome {
    /// Recorded; the engine reruns from the decision's step.
    Resume,
    /// The task was settled; `detail` says how.
    Settled { detail: String },
    /// The task is parked for a human; `detail` says why.
    Escalated { detail: String },
}

impl fmt::Display for FinalRecoveryCheckpointOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resume => formatter.write_str("resume"),
            Self::Settled { detail } => write!(formatter, "settled: {detail}"),
            Self::Escalated { detail } => write!(formatter, "escalated: {detail}"),
        }
    }
}

impl<'de> Deserialize<'de> for FinalRecoveryCheckpointOutcome {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum TypedOutcome {
            Resume,
            Settled { detail: String },
            Escalated { detail: String },
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StoredOutcome {
            Legacy(String),
            Typed(TypedOutcome),
        }

        fn legacy_detail<'a>(stored: &'a str, prefix: &str) -> Option<&'a str> {
            stored
                .strip_prefix(prefix)
                .map(|detail| detail.strip_prefix(' ').unwrap_or(detail))
        }

        match StoredOutcome::deserialize(deserializer)? {
            StoredOutcome::Typed(TypedOutcome::Resume) => Ok(Self::Resume),
            StoredOutcome::Typed(TypedOutcome::Settled { detail }) => Ok(Self::Settled { detail }),
            StoredOutcome::Typed(TypedOutcome::Escalated { detail }) => {
                Ok(Self::Escalated { detail })
            }
            StoredOutcome::Legacy(stored) if stored == "resume" => Ok(Self::Resume),
            StoredOutcome::Legacy(stored) => {
                if let Some(detail) = legacy_detail(&stored, "settled:") {
                    Ok(Self::Settled {
                        detail: detail.to_string(),
                    })
                } else if let Some(detail) = legacy_detail(&stored, "escalated:") {
                    Ok(Self::Escalated {
                        detail: detail.to_string(),
                    })
                } else {
                    Err(serde::de::Error::custom(format!(
                        "unknown final-recovery checkpoint outcome '{stored}'"
                    )))
                }
            }
        }
    }
}
