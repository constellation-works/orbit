//! Private, versioned consumer ledger; no task fields or workflow authority.
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::{OrbitError, fs::io};
use orbit_types::assistance::{
    self, AssistanceCriterionRef, AssistanceReason, AssistanceResult, AssistanceValue,
};
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;

pub(super) fn quality_json_error(_: serde_json::Error) -> OrbitError {
    OrbitError::Store("task quality ledger encoding is invalid".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQualityHistory {
    pub schema_version: u32,
    pub snapshots: Vec<TaskQualitySnapshot>,
    /// Derived on every read, never trusted from the file.
    pub current_id: Option<String>,
}

impl Default for TaskQualityHistory {
    fn default() -> Self {
        Self {
            schema_version: 1,
            snapshots: Vec::new(),
            current_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQualitySnapshot {
    pub id: String,
    pub source_digest: String,
    pub policy_epoch: String,
    pub collected_at: DateTime<Utc>,
    pub admission_started: bool,
    pub work_id: Option<String>,
    pub coverage_gap: Option<AssistanceReason>,
    pub result: Option<AssistanceResult>,
    pub annotations: Vec<TaskQualityAnnotation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQualityAnnotation {
    pub id: String,
    pub rule_id: String,
    pub criterion_refs: Vec<AssistanceCriterionRef>,
    pub dismissal: Option<TaskQualityDismissal>,
}

/// Fixed, reasoned feedback; no arbitrary text, model explanation or secret
/// channel. Feedback is an author's observation, never evaluation ground truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskQualityDismissalReason {
    ValidInvestigation,
    CoherentSharedOutcome,
    ObservableEvidenceSpecified,
    RequirementsCompatible,
    FalsePositive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQualityDismissal {
    pub reason: TaskQualityDismissalReason,
    pub at: DateTime<Utc>,
}

pub(super) fn quality_ledger_path(runtime: &OrbitRuntime, id: &str) -> Result<PathBuf, OrbitError> {
    let workspace = assistance::assistance_digest(runtime.workspace_id()?.as_bytes());
    let subject = assistance::assistance_digest(id.as_bytes());
    Ok(runtime
        .global_root()
        .join("state/task-quality/v1")
        .join(&workspace[7..])
        .join(format!("{}.json", &subject[7..])))
}

pub(super) fn read_quality_history(path: &Path) -> Result<TaskQualityHistory, OrbitError> {
    let file = match io::open_read_only_no_follow(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TaskQualityHistory::default());
        }
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > 8 * 1024 * 1024 {
        return Err(OrbitError::Store(
            "task quality ledger exceeds its bound".into(),
        ));
    }
    let history: TaskQualityHistory = serde_json::from_reader(file).map_err(quality_json_error)?;
    if history.schema_version != 1 || history.snapshots.len() > 32 {
        return Err(OrbitError::Store("unsupported task quality ledger".into()));
    }
    Ok(history)
}

pub(super) fn write_quality_history(
    path: &Path,
    history: &TaskQualityHistory,
) -> Result<(), OrbitError> {
    io::atomic_write_text(
        path,
        &serde_json::to_string(history).map_err(quality_json_error)?,
    )?;
    Ok(())
}

pub(super) fn with_quality_history<T>(
    path: &Path,
    body: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    io::with_exclusive_file_lock_options(
        path,
        "task quality",
        io::FileLockOptions {
            timeout: std::time::Duration::from_millis(50),
            ..Default::default()
        },
        body,
    )
}

pub(super) fn quality_annotations(
    snapshot: &TaskQualitySnapshot,
    result: &AssistanceResult,
) -> Result<Vec<TaskQualityAnnotation>, OrbitError> {
    result
        .answers
        .iter()
        .filter(|(_, answer)| answer.value == AssistanceValue::Predicate(true))
        .map(|(name, answer)| {
            let rule = name.split('/').next().unwrap_or(name);
            let id = assistance::assistance_digest(
                &serde_json::to_vec(&(
                    "task-quality-annotation/1",
                    &snapshot.id,
                    rule,
                    &answer.criterion_refs,
                ))
                .map_err(quality_json_error)?,
            );
            Ok(TaskQualityAnnotation {
                dismissal: snapshot
                    .annotations
                    .iter()
                    .find(|a| a.id == id)
                    .and_then(|a| a.dismissal.clone()),
                id,
                rule_id: rule.into(),
                criterion_refs: answer.criterion_refs.clone(),
            })
        })
        .collect()
}
