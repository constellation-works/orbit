//! Retained review report revisions [ORB-14192].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ReviewValidation, ReviewVerdict};

/// Task artifact the artifact store keeps beside [`REVIEW_REPORT_ARTIFACT`]:
/// every accepted report revision's verdict and validation records. Only the
/// store writes it, in the same manifest write that replaces the report.
pub const REVIEW_REPORT_HISTORY_ARTIFACT: &str = "review-report-history.json";

/// Version of [`ReviewReportHistory`].
pub const REVIEW_REPORT_HISTORY_VERSION: u32 = 1;

/// How many report revisions one task's history keeps.
pub const REVIEW_REPORT_HISTORY_LIMIT: usize = 64;

/// One report revision the host accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReportRevision {
    pub attempt_id: String,
    /// SHA-256 of the report bytes; the bytes stay in the immutable blob.
    pub sha256: String,
    pub observed_at: DateTime<Utc>,
    pub recorded_by: String,
    pub verdict: ReviewVerdict,
    #[serde(default)]
    pub validation: Vec<ReviewValidation>,
}

/// The report revisions of one task, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReportHistory {
    pub schema_version: u32,
    #[serde(default)]
    pub revisions: Vec<ReviewReportRevision>,
}

impl Default for ReviewReportHistory {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_REPORT_HISTORY_VERSION,
            revisions: Vec::new(),
        }
    }
}

impl ReviewReportHistory {
    /// Read a stored history; another version or malformed content is an
    /// error, never an empty history.
    pub fn parse(content: &[u8]) -> Result<Self, String> {
        let history: Self = serde_json::from_slice(content)
            .map_err(|error| format!("{REVIEW_REPORT_HISTORY_ARTIFACT} is unreadable: {error}"))?;
        if history.schema_version != REVIEW_REPORT_HISTORY_VERSION {
            return Err(format!(
                "{REVIEW_REPORT_HISTORY_ARTIFACT} has schema_version {}; this build reads \
                 version {REVIEW_REPORT_HISTORY_VERSION}",
                history.schema_version
            ));
        }
        Ok(history)
    }

    /// Append `revision`. Re-recording a revision of the same attempt with
    /// the same bytes changes nothing and returns `false`, so a retried put
    /// after a lost response is idempotent. At the limit the oldest revision
    /// of another attempt makes room; a single attempt that fills the
    /// history is refused rather than losing its own obligations.
    pub fn record(&mut self, revision: ReviewReportRevision) -> Result<bool, String> {
        if self
            .revisions
            .iter()
            .any(|kept| kept.attempt_id == revision.attempt_id && kept.sha256 == revision.sha256)
        {
            return Ok(false);
        }
        if self.revisions.len() >= REVIEW_REPORT_HISTORY_LIMIT {
            let Some(oldest_other) = self
                .revisions
                .iter()
                .position(|kept| kept.attempt_id != revision.attempt_id)
            else {
                return Err(format!(
                    "attempt {} already recorded {REVIEW_REPORT_HISTORY_LIMIT} report revisions; \
                     settle it or admit a fresh review",
                    revision.attempt_id
                ));
            };
            self.revisions.remove(oldest_other);
        }
        self.revisions.push(revision);
        Ok(true)
    }

    /// Revisions recorded for `attempt_id`, oldest first.
    pub fn for_attempt<'a>(
        &'a self,
        attempt_id: &'a str,
    ) -> impl Iterator<Item = &'a ReviewReportRevision> + 'a {
        self.revisions
            .iter()
            .filter(move |revision| revision.attempt_id == attempt_id)
    }
}
