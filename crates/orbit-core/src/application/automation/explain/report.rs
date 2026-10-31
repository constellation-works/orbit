use chrono::{DateTime, Utc};
use orbit_automation::condition::ClauseNode;
use serde::Serialize;

use super::super::condition::ConditionStatus;
use super::JobStepsExplain;

/// The `orbit.automation.explain` payload: a condition's report, or a job's
/// auto-task steps for `job:<name>`.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AutomationExplain {
    Condition(Box<ConditionExplain>),
    Job(Box<JobStepsExplain>),
}

/// The explain report for a routine's or auto-task's condition.
#[derive(Debug, Serialize)]
pub struct ConditionExplain {
    pub definition: ExplainedDefinition,
    pub scope: &'static str,
    /// Where the condition sits in its file.
    pub clause: &'static str,
    pub when: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rearm: Option<String>,
    /// Task scope: the store query subjects come from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefilter: Option<ExplainPrefilter>,
    pub cross_reads: ExplainCrossReads,
    pub ownership: ExplainOwnership,
    pub evaluated_at: String,
    pub subjects: ExplainSubjects,
    pub matches: Vec<ExplainMatch>,
    pub closest_misses: Vec<ExplainMiss>,
    /// The clause tree for one subject; absent when nothing was selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<ExplainSubject>,
    /// Values the board could not read; expressions reading them are unknown.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<String>,
    /// The durable state: ownership, arm states and holds, the stall, recent
    /// fires with their upstream chain, and `self`.
    pub tracking: ConditionStatus,
}

#[derive(Debug, Serialize)]
pub struct ExplainedDefinition {
    /// `routine`, `auto-task` or `job`.
    pub kind: &'static str,
    pub name: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplainPrefilter {
    pub statuses: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainCrossReads {
    pub tasks: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainOwnership {
    /// `owned_here`, `owned_elsewhere` or `ownership_unresolved`.
    pub verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_machine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub this_host: Option<String>,
    /// Whether this checkout is a replica, which never evaluates a
    /// workspace or task condition.
    pub replica: bool,
}

#[derive(Debug, Serialize)]
pub struct ExplainSubjects {
    /// Subjects the scope selected (task scope: the prefilter's tasks).
    pub selected: usize,
    /// Subjects evaluated, at most 200.
    pub evaluated: usize,
    /// Whether the cap left selected subjects unevaluated.
    pub truncated: bool,
    pub matched: usize,
    pub missed: usize,
    pub unknown: usize,
}

#[derive(Debug, Serialize)]
pub struct ExplainMatch {
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `armed`, `fired`, or `untracked` before the first pass records it.
    pub arm: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub true_since: Option<DateTime<Utc>>,
    /// Minutes of `for_minutes` still to hold before a fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold_remaining_minutes: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ExplainMiss {
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The one top-level clause that is not true.
    pub clause: String,
    /// `false` or `unknown`.
    pub value: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExplainSubject {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Task scope: whether the prefilter selects this task. A task outside
    /// it is never a subject, whatever `when` says.
    pub in_prefilter: bool,
    pub value: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub when: ClauseNode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rearm: Option<ClauseNode>,
}
