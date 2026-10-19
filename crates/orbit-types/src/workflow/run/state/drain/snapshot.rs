//! Drain controls and checkpoints carried by a pipeline run.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    DrainAdmissionForecast, DrainAdmissionPass, DrainAdmissionsStop, DrainApprovalReport,
    DrainCancelRequest, DrainPassCounters, DrainPassObservation, DrainWindowExtension,
    DrainWorkerLimit, PullAdmissionFit, PullAuthRecovery, PullBreaker, PullCrewPreflight,
    PullSinglePass, PullWindowClose,
};

/// Drain controls, pull admission checkpoints, and delivery forge holds carried
/// by a pipeline run. Missing records retain the submitted/default behavior.
///
/// [`crate::workflow::PipelineState::pull_drain`] flattens these fields into `state.json`;
/// the Rust grouping does not add an object or rename a persisted key.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PullDrainState {
    /// Live worker ceiling for a bounded auto drain, when an operator has
    /// adjusted it [ORB-11253]. Absent means the submitted input still
    /// governs. Like `child_dispatches` this survives terminalization: it is
    /// the evidence of what the run was actually admitting under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_worker_limit: Option<DrainWorkerLimit>,
    /// Operator stop of *new* admissions on a bounded auto drain [ORB-11283].
    /// Absent means the drain is still admitting under its window and ceiling.
    /// Like `drain_worker_limit` this survives terminalization: it is how a
    /// finished coordinator is distinguished from cancellation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_admissions_stop: Option<DrainAdmissionsStop>,
    /// Operator extension of this drain's window [ORB-15383]. Absent means the
    /// deadline its `open_window` step stamped still governs. Survives
    /// terminalization like `drain_worker_limit`, as evidence of the window
    /// the run actually admitted under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_window_extension: Option<DrainWindowExtension>,
    /// A graceful cancel this pull drain is carrying out: present while it
    /// waits for its launched leaves, and kept once it ends `cancelled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_cancel: Option<DrainCancelRequest>,
    /// The provider preflight a pull drain took when its window opened.
    /// Survives terminalization: it is how `run show` reports which crews
    /// the drain could not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_crew_preflight: Option<PullCrewPreflight>,
    /// Recovery per released auth incident; old incidents stay acknowledged on resume.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pull_auth_recovery: BTreeMap<String, PullAuthRecovery>,
    /// The single admission pass of a pull drain submitted without a window,
    /// once it has started. Survives terminalization and resume, so the pass
    /// is never taken twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_single_pass: Option<PullSinglePass>,
    /// The pass that closed this timed pull drain's window [ORB-15417]: once
    /// present, no extension reopens it. Survives terminalization and resume,
    /// so a retried pass never admits past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_window_close: Option<PullWindowClose>,
    /// This pull drain's failure breaker while it is open or half-open
    /// [ORB-15639]; absent while closed. Survives terminalization, so `run
    /// show` reports the breaker a drain ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_breaker: Option<PullBreaker>,
    /// What this pull drain's host load forecast last had room for, under
    /// `machine.auto_concurrency` `shadow` or `enforce`; absent with `off` or
    /// before the host has resource profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_admission_fit: Option<PullAdmissionFit>,
    /// What this drain's last admission pass left waiting. Survives
    /// terminalization: it is how `run show` reports the backlog a finished
    /// drain never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_pass: Option<DrainAdmissionPass>,
    /// The most recent local pass that built a forecast. Idle passes keep
    /// this observation, including its timestamp, decisions and peaks.
    /// Survives terminalization; absent before any forecast was built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_last_forecast: Option<DrainAdmissionForecast>,
    /// What limited each of this drain's admission passes, counted over its
    /// whole lineage: `drain_last_pass` keeps only the latest. Survives
    /// terminalization and resume; absent before the first counted pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_pass_counters: Option<DrainPassCounters>,
    /// What this `--approve-proposed` drain approved and held. Survives
    /// terminalization like `drain_last_pass`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain_approvals: Option<DrainApprovalReport>,
    /// The push this run held at because the forge kept refusing it
    /// [ORB-14617]. Present only while the run's last outcome is that hold:
    /// finalization replaces it with the run's own result, and a resume
    /// seeded from a held run reads it to keep the lineage's `held_since`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold: Option<crate::workflow::ForgeUnavailableHold>,
    /// When the clock finished expiring this run's forge hold. A recorded
    /// expiry stops later ticks from acting again; manual resume retains the
    /// hold and checkpoints, but clears this run-local acknowledgement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge_hold_expired_at: Option<DateTime<Utc>>,
}

impl PullDrainState {
    /// Replace the current pass while retaining the latest built forecast.
    pub fn record_admission_pass(&mut self, pass: DrainAdmissionPass) {
        let built = pass
            .admission_forecast
            .as_ref()
            .filter(|forecast| forecast.forecast_at.is_some());
        // Seed history from older checkpoints before an idle pass replaces it.
        let previous = self
            .drain_last_pass
            .as_ref()
            .and_then(|pass| pass.admission_forecast.as_ref())
            .filter(|forecast| forecast.forecast_at.is_some());
        if let Some(forecast) = built.or(previous) {
            self.drain_last_forecast = Some(forecast.clone());
        }
        self.drain_last_pass = Some(pass);
    }

    /// Count one admission pass in [`Self::drain_pass_counters`].
    pub fn count_admission_pass(&mut self, observation: &DrainPassObservation, at: DateTime<Utc>) {
        self.drain_pass_counters
            .get_or_insert_with(|| DrainPassCounters {
                since: at,
                ..DrainPassCounters::default()
            })
            .count(observation, at);
    }
}
