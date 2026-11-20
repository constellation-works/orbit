//! Standing drain phase 1 (ORB-25965): the workspace's drain intent and the
//! admission windows its drains held.
//!
//! A hand stop records `paused` with who stopped it and when; a hand start
//! records `on`. A workspace that has never recorded one is `off`. Phase 1
//! only records and reports the intent: nothing reads it to start, extend or
//! refuse a drain, so recording it never changes drain behaviour.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{DrainIntentRecord, DrainIntentState};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// The drain intent as readiness and the dashboard report it: `intent` is
/// `off`, `on` or `paused`; `by`, `source`, `at` and `reason` say who set it,
/// from which surface, when and why (absent while `off`).
#[must_use]
pub fn drain_intent_json(record: Option<&DrainIntentRecord>) -> Value {
    match record {
        None => json!({ "intent": "off" }),
        Some(record) => json!({
            "intent": record.intent.as_str(),
            "by": record.actor,
            "source": record.source,
            "at": record.updated_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "reason": record.reason,
        }),
    }
}

/// One drain's admission window: from when it started to when it stopped
/// admitting (its deadline, an admissions stop or cancel, or its end,
/// whichever came first). A drain still admitting ends at `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DrainAdmissionWindow {
    pub(crate) opened_at: DateTime<Utc>,
    pub(crate) closed_at: DateTime<Utc>,
}

impl OrbitRuntime {
    /// The admission windows of this workspace's own auto drains that were
    /// open at any time since `since`, unclipped and in no particular order.
    /// A drain that never started held no window.
    pub(crate) fn drain_admission_windows_since(
        &self,
        since: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Vec<DrainAdmissionWindow>, OrbitError> {
        let runs =
            self.stores()
                .jobs()
                .list_job_runs_filtered(&orbit_store::contracts::JobRunQuery {
                    job_id: Some(orbit_types::workflow::WORKSPACE_AUTO_PIPELINE_JOB.to_string()),
                    recency_since: Some(since),
                    include_steps: false,
                    input_keys: Some(&[]),
                    ..Default::default()
                })?;
        let mut windows = Vec::with_capacity(runs.len());
        for run in runs {
            let Some(opened_at) = run.started_at else {
                continue;
            };
            let mut closed_at = if run.state.is_terminal() {
                run.finished_at.unwrap_or(opened_at)
            } else {
                now
            };
            if let Some(state) = self.read_run_state(&run.run_id)? {
                let controls = &state.pull_drain;
                let ends = [
                    super::drain_deadline(&state),
                    controls
                        .drain_admissions_stop
                        .as_ref()
                        .map(|stop| stop.stopped_at),
                    controls
                        .drain_cancel
                        .as_ref()
                        .map(|cancel| cancel.requested_at),
                ];
                for end in ends.into_iter().flatten() {
                    closed_at = closed_at.min(end);
                }
            }
            if closed_at > opened_at {
                windows.push(DrainAdmissionWindow {
                    opened_at,
                    closed_at,
                });
            }
        }
        Ok(windows)
    }

    /// This workspace's recorded drain intent; `None` is `off`.
    pub fn workspace_drain_intent(&self) -> Result<Option<DrainIntentRecord>, OrbitError> {
        self.stores().jobs().drain_intent()
    }

    /// Record `on` for a drain submitted under `trigger` when a person or an
    /// agent asked for it (CLI, dashboard or MCP); a routine or child
    /// submission records nothing.
    pub(crate) fn record_drain_start_intent(
        &self,
        trigger: orbit_types::workflow::JobRunTriggerKind,
    ) {
        use orbit_types::workflow::JobRunTriggerKind;
        match trigger {
            JobRunTriggerKind::Cli | JobRunTriggerKind::Dashboard | JobRunTriggerKind::Mcp => {
                self.record_hand_drain_intent(DrainIntentState::On, trigger.as_str(), None);
            }
            JobRunTriggerKind::Routine | JobRunTriggerKind::Child => {}
        }
    }

    /// Record a hand start (`on`) or stop (`paused`) from `source`.
    ///
    /// The acting identity is this process's actor label, so a stop names the
    /// person or agent behind it rather than only the surface. Recording is
    /// best effort: the start or stop it describes has already happened, and a
    /// failed write is logged rather than turning that success into an error.
    pub(crate) fn record_hand_drain_intent(
        &self,
        intent: DrainIntentState,
        source: &str,
        reason: Option<&str>,
    ) {
        let record = DrainIntentRecord {
            intent,
            actor: self.actor_label().to_string(),
            source: source.to_string(),
            reason: reason.map(str::to_string),
            updated_at: Utc::now(),
        };
        if let Err(error) = self.stores().jobs().record_drain_intent(&record) {
            tracing::warn!(
                intent = intent.as_str(),
                source,
                %error,
                "drain intent not recorded"
            );
        }
    }
}
