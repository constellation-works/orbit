//! Compatibility codec for the failure-handoff history note and paired comment.

/// The failed step relevant to attribution, retaining other step names verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureHandoffStep {
    /// The candidate could not be synchronized with its base.
    SyncBase,
    /// Any other pipeline step, including a name unknown to this version.
    Other(String),
}

impl From<&str> for FailureHandoffStep {
    fn from(step: &str) -> Self {
        match step {
            "sync_base" => Self::SyncBase,
            other => Self::Other(other.to_string()),
        }
    }
}

/// Conflict evidence recorded at the failure-handoff task-history boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedCandidateConflict {
    /// Absent when a legacy note did not report a step.
    pub failed_step: Option<FailureHandoffStep>,
    /// Raw recorded paths; consumers retain their own selector normalization.
    pub conflicting_paths: Vec<String>,
}

/// Encodes the existing note bytes and decodes partial historical notes.
///
/// This is a text compatibility boundary, not a new persisted metadata shape.
pub struct FailureHandoffHistoryNote;

impl FailureHandoffHistoryNote {
    /// Construct the note used by both the status update and paired comment.
    pub fn encode(
        conflict: &RecordedCandidateConflict,
        pr_number: &str,
        run_id: &str,
        original_base_sha: &str,
        target_base_sha: &str,
    ) -> String {
        let failed_step = match &conflict.failed_step {
            Some(FailureHandoffStep::SyncBase) => "sync_base",
            Some(FailureHandoffStep::Other(step)) => step.as_str(),
            None => "-",
        };
        let paths = if conflict.conflicting_paths.is_empty() {
            "none reported".to_string()
        } else {
            conflict.conflicting_paths.join(", ")
        };
        format!(
            "failure handoff published PR #{pr_number}: run={run_id}, failed_step={failed_step}, \
             original_base={original_base_sha}, target_base={target_base_sha}, conflicts={paths}"
        )
    }

    /// Read only the attribution fields, without requiring the other writer fields.
    /// Missing evidence stays absent; paths stop at the end of their first line.
    pub fn decode(note: Option<&str>) -> RecordedCandidateConflict {
        let text = note.unwrap_or_default();
        let failed_step = text.find("failed_step=").and_then(|start| {
            let value = text[start + "failed_step=".len()..]
                .split([',', ' ', ';', '\n'])
                .next()?;
            (!value.is_empty() && value != "-").then(|| value.into())
        });
        let conflicting_paths = text
            .rsplit_once("conflicts=")
            .map(|(_, paths)| {
                paths
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split(", ")
                    .filter(|path| *path != "none reported")
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        RecordedCandidateConflict {
            failed_step,
            conflicting_paths,
        }
    }
}
