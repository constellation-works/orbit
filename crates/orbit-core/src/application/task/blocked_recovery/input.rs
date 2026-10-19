//! The backstop's run input, tick report and view, and what a recovery run's first step established.

use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::final_recovery::FinalRecoveryTaskRevision;
use super::super::retained_candidate::RetainedCandidate;
use super::EPISODE_KEY_FIELD;
use super::episode::{BlockEpisode, EpisodeDisposition};

/// The run input the backstop dispatches with; also how a recovery run finds
/// its episode again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRecoveryInput {
    /// The blocked task.
    pub task_id: String,
    /// [`BlockEpisode::key`] of the episode being recovered.
    pub episode_key: String,
    /// [`BlockSource::as_str`](super::BlockSource::as_str).
    pub block_source: String,
    /// The run that failed, when known.
    pub failed_run_id: Option<String>,
    /// The task revision observed when the recovery was dispatched.
    pub observed: FinalRecoveryTaskRevision,
}

impl BlockedRecoveryInput {
    /// The run-input JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "task_id": self.task_id,
            EPISODE_KEY_FIELD: self.episode_key,
            "block_source": self.block_source,
            "failed_run_id": self.failed_run_id.clone().unwrap_or_default(),
            "observed_status": self.observed.status.to_string(),
            "observed_updated_at": self
                .observed
                .updated_at
                .to_rfc3339_opts(SecondsFormat::Nanos, true),
            "observed_lifecycle_digest": self
                .observed
                .lifecycle_digest
                .clone()
                .unwrap_or_default(),
        })
    }

    /// Read a run input written by [`Self::to_json`].
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let text = |field: &str| -> Result<String, String> {
            value
                .get(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("blocked-task recovery input lacks `{field}`"))
        };
        let status = text("observed_status")?
            .parse::<TaskStatus>()
            .map_err(|error| format!("observed_status: {error}"))?;
        let updated_at = DateTime::parse_from_rfc3339(&text("observed_updated_at")?)
            .map_err(|error| format!("observed_updated_at: {error}"))?
            .with_timezone(&Utc);
        Ok(Self {
            task_id: text("task_id")?,
            episode_key: text(EPISODE_KEY_FIELD)?,
            block_source: text("block_source")?,
            failed_run_id: text("failed_run_id").ok(),
            observed: FinalRecoveryTaskRevision {
                status,
                updated_at,
                lifecycle_digest: text("observed_lifecycle_digest").ok(),
            },
        })
    }
}

/// What one backstop tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockedRecoveryTick {
    /// Why the tick did nothing, when it stood down.
    pub skipped: Option<String>,
    /// `(task_id, run_id)` for each recovery dispatched.
    pub dispatched: Vec<(String, String)>,
    /// Tasks whose recovery run ended without a decision and were escalated.
    pub settled: Vec<String>,
}

/// One blocked task as the backstop sees it, for `orbit doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRecoveryView {
    /// The blocked episode.
    pub episode: BlockEpisode,
    /// What the backstop does about it.
    pub disposition: EpisodeDisposition,
}

/// What a recovery run's first step established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BlockedRecoveryPreparation {
    /// The episode is no longer the task's untouched block; do nothing.
    Skip {
        /// Why.
        reason: String,
    },
    /// The agent's input.
    Ready(Box<PreparedBlockedRecovery>),
}

/// The final-recovery agent's view of a blocked episode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedBlockedRecovery {
    /// The run's private detached checkout of `base_sha`.
    pub(crate) checkout: PathBuf,
    /// Base ref a `complete_no_diff` commit must be reachable from.
    pub(crate) base_ref: String,
    /// The commit `base_ref` resolved to.
    pub(crate) base_sha: String,
    /// The run that failed, or the recovery run when none is known.
    pub(crate) failed_run_id: String,
    /// The failed step, or the block source when no run step is known.
    pub(crate) failed_step_id: String,
    /// The failed run's job, when known.
    pub(crate) job_id: Option<String>,
    /// The failure as recorded.
    pub(crate) error_message: String,
    /// The failed run's worktree, when it is still on this machine.
    pub(crate) retained_candidate: RetainedCandidate,
}
