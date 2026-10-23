//! Acceptance criteria that only the deployed build on a named host can
//! evidence.
//!
//! The task pilot types such a criterion. An implementer that cannot meet it
//! before merge hands it off as an [`OwedCriterion`] instead of failing, and
//! the owner admits the claim only for a criterion its pilot typed. The
//! delivery then settles in `review`, and an operator records each owed
//! criterion met or failed once the deployed build is measured.

use crate::identity::is_automation_actor;
use crate::task::TaskHistoryEntry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The implementer output field that carries the owed criteria.
pub const OWED_CRITERIA_FIELD: &str = "owed_criteria";

/// Task history event naming the criteria a delivery owes after deploy. Its
/// note is a [`PostDeployOwed`].
pub const POST_DEPLOY_OWED_EVENT: &str = "post_deploy_owed";

/// Task history event recording an operator's measurement of one owed
/// criterion. Its note is a [`PostDeployResult`].
pub const POST_DEPLOY_RECORDED_EVENT: &str = "post_deploy_recorded";

/// First-line prefix of an operator comment that records a result:
/// `post-deploy: met <N>` or `post-deploy: failed <N>`, evidence below it.
pub const POST_DEPLOY_HEADER: &str = "post-deploy:";

/// One acceptance criterion a delivery owes until the deployed build is
/// measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwedCriterion {
    /// 1-based index of the task's acceptance criteria.
    pub criterion: usize,
    /// The host whose deployed build the measurement runs against.
    pub host: String,
    /// The exact measurement command.
    pub command: String,
    /// The value the measurement must meet, such as `p95 <= 150 ms`.
    pub target: String,
    /// What the candidate itself showed for the criterion, if anything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub candidate_evidence: String,
}

impl OwedCriterion {
    /// The owed criteria in an implementer's `output`: empty when it names
    /// none, refused when the field is malformed or names a criterion twice.
    pub fn from_output(output: &Value) -> Result<Vec<Self>, String> {
        let value = match output.get(OWED_CRITERIA_FIELD) {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(value) => value,
        };
        let owed: Vec<Self> = serde_json::from_value(value.clone()).map_err(|error| {
            format!(
                "`{OWED_CRITERIA_FIELD}` must be `[{{\"criterion\": N, \"host\": ..., \
                 \"command\": ..., \"target\": ..., \"candidate_evidence\": ...}}]`: {error}"
            )
        })?;
        Self::check_all(&owed)?;
        Ok(owed)
    }

    /// Refuse a list that names a criterion twice or leaves a required field
    /// empty. A typed handoff carries the list without the output parse.
    pub fn check_all(owed: &[Self]) -> Result<(), String> {
        let mut seen = Vec::new();
        for entry in owed {
            if entry.criterion == 0 {
                return Err(format!(
                    "`{OWED_CRITERIA_FIELD}` criterion must be a 1-based acceptance criterion index"
                ));
            }
            for (field, value) in [
                ("host", &entry.host),
                ("command", &entry.command),
                ("target", &entry.target),
            ] {
                if value.trim().is_empty() {
                    return Err(format!(
                        "`{OWED_CRITERIA_FIELD}` criterion {} needs a non-empty {field}",
                        entry.criterion
                    ));
                }
            }
            if seen.contains(&entry.criterion) {
                return Err(format!(
                    "`{OWED_CRITERIA_FIELD}` names criterion {} twice",
                    entry.criterion
                ));
            }
            seen.push(entry.criterion);
        }
        Ok(())
    }
}

/// The note of a [`POST_DEPLOY_OWED_EVENT`]: the run that delivered and the
/// criteria it owes. The newest one is the task's current record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostDeployOwed {
    pub run_id: String,
    pub criteria: Vec<OwedCriterion>,
}

/// An operator's measurement of an owed criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostDeployOutcome {
    Met,
    Failed,
}

impl PostDeployOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Met => "met",
            Self::Failed => "failed",
        }
    }
}

/// The note of a [`POST_DEPLOY_RECORDED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostDeployResult {
    pub criterion: usize,
    pub outcome: PostDeployOutcome,
    pub evidence: String,
}

/// The outcome and criterion a comment's first line records, `None` when it
/// is not a [`POST_DEPLOY_HEADER`] line, and an error when it is one that
/// does not read `post-deploy: met|failed <N>`.
pub fn post_deploy_header(line: &str) -> Option<Result<(PostDeployOutcome, usize), String>> {
    let rest = line.trim().strip_prefix(POST_DEPLOY_HEADER)?;
    let mut words = rest.split_whitespace();
    let outcome = match words.next() {
        Some("met") => Some(PostDeployOutcome::Met),
        Some("failed") => Some(PostDeployOutcome::Failed),
        _ => None,
    };
    let criterion = words
        .next()
        .and_then(|word| word.parse::<usize>().ok())
        .filter(|criterion| *criterion > 0);
    Some(match (outcome, criterion, words.next()) {
        (Some(outcome), Some(criterion), None) => Ok((outcome, criterion)),
        _ => Err(format!(
            "a post-deploy record's first line reads `{POST_DEPLOY_HEADER} met <N>` or \
             `{POST_DEPLOY_HEADER} failed <N>`, where N is the owed criterion"
        )),
    })
}

/// An operator's recorded result for one owed criterion.
pub struct RecordedPostDeploy {
    /// The operator's measurement.
    pub result: PostDeployResult,
    /// Persisted operator attribution.
    pub by: String,
    /// When the measurement was recorded.
    pub at: DateTime<Utc>,
}

/// The task's current owed record: the newest [`POST_DEPLOY_OWED_EVENT`],
/// with the latest operator result after it for each criterion.
pub struct PostDeployRecord {
    /// The latest accepted owed declaration.
    pub owed: PostDeployOwed,
    /// Latest operator results, aligned with the owed criteria.
    pub recorded: Vec<Option<RecordedPostDeploy>>,
}

impl PostDeployRecord {
    /// Read the current declaration and subsequent operator measurements.
    pub fn read(history: &[TaskHistoryEntry]) -> Option<Self> {
        let (start, owed) = history
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| {
                (entry.event == POST_DEPLOY_OWED_EVENT)
                    .then(|| serde_json::from_str::<PostDeployOwed>(entry.note.as_deref()?).ok())
                    .flatten()
                    .map(|owed| (index, owed))
            })?;
        let mut recorded: Vec<Option<RecordedPostDeploy>> =
            owed.criteria.iter().map(|_| None).collect();
        for entry in &history[start + 1..] {
            if entry.event != POST_DEPLOY_RECORDED_EVENT || is_automation_actor(&entry.by) {
                continue;
            }
            let Some(result) = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<PostDeployResult>(note).ok())
            else {
                continue;
            };
            if let Some(slot) = owed
                .criteria
                .iter()
                .position(|owed| owed.criterion == result.criterion)
            {
                recorded[slot] = Some(RecordedPostDeploy {
                    result,
                    by: entry.by.clone(),
                    at: entry.at,
                });
            }
        }
        Some(Self { owed, recorded })
    }

    /// Criteria no operator has recorded met.
    pub fn outstanding(&self) -> Vec<usize> {
        self.owed
            .criteria
            .iter()
            .zip(&self.recorded)
            .filter(|(_, recorded)| {
                !recorded
                    .as_ref()
                    .is_some_and(|recorded| recorded.result.outcome == PostDeployOutcome::Met)
            })
            .map(|(owed, _)| owed.criterion)
            .collect()
    }
}
