//! Gap fit in the backlog forecast's simulated passes [ORB-18689].
//!
//! Under `enforce`, the drain asks gap fit about each candidate that passed
//! every other gate, against the host load forecast (`gap_fit.rs`). The
//! simulated pass asks the same [`GapFit`], through
//! [`GapFit::admits_priced`], against a [`simulated_forecast`] of the
//! simulated running set: the same fit rule, thresholds, profiles and
//! fallback chain, the idle-host admission and the head starvation guard,
//! with the head's wait measured in simulated time. Each task is priced once
//! per forecast with the drain's own lookup, `task_lookup`.
//!
//! The host's size and baseline are captured once, so the curve changes only
//! as simulated deliveries age, start and end. A pass that gap fit held
//! while a slot was free is retried every [`RESOURCE_RECHECK_HOURS`] while a
//! running delivery's profile still changes, as the live drain's next pass
//! would see the curve fall; once every running delivery is past its
//! profile, only a completion can make room.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_types::task::Task;
use orbit_types::workflow::GapFitHead;

use super::super::gap_fit::{GapFit, GapFitMode, HeadWait, task_lookup};
use super::ForecastTaskSet;
use crate::runtime::host_resource::forecast::{
    HostLoadForecast, SimulatedDelivery, SimulatedHost, simulated_forecast,
};
use crate::runtime::host_resource::profile::{ProfileMatch, ProfileSet, ProfileSource};

/// How often a pass that gap fit held is retried while the running set's
/// curve still changes: five simulated minutes.
pub(crate) const RESOURCE_RECHECK_HOURS: f64 = 5.0 / 60.0;

/// What the forecast captured of the execution host, and the gap-fit
/// settings its drain admits under.
#[derive(Clone)]
pub(crate) struct ForecastHostEvidence {
    pub(crate) host: SimulatedHost,
    pub(crate) profiles: Arc<ProfileSet>,
    pub(crate) workspace_id: String,
    pub(crate) admit_cpu_percent: u8,
    pub(crate) admit_memory_percent: u8,
    pub(crate) head_max_wait_minutes: u32,
    pub(crate) head_stuck_passes: u32,
    pub(crate) horizon_minutes: u32,
}

/// The frozen resource-fit inputs of one forecast.
pub(super) struct ResourceFit {
    evidence: ForecastHostEvidence,
    /// Per task: the profile the drain's lookup finds for it.
    priced: Vec<ProfileMatch>,
}

impl ResourceFit {
    pub(super) fn with_forecast_generated_nodes(&self, templates: &[usize]) -> Self {
        Self {
            evidence: self.evidence.clone(),
            priced: templates
                .iter()
                .map(|index| self.priced[*index].clone())
                .collect(),
        }
    }

    pub(super) fn new(evidence: ForecastHostEvidence, tasks: &[Task]) -> Self {
        let priced = tasks
            .iter()
            .map(|task| {
                evidence.profiles.lookup(&task_lookup(
                    &evidence.workspace_id,
                    task,
                    &evidence.profiles,
                ))
            })
            .collect();
        Self { evidence, priced }
    }

    pub(super) fn profile_source(&self, index: usize) -> ProfileSource {
        self.priced[index].source
    }

    /// The host load forecast at `now`, `at_hours` into the simulation,
    /// with `running` admitted at their `admitted_at` hours.
    fn forecast(
        &self,
        tasks: &[Task],
        running: &ForecastTaskSet,
        admitted_at: &[f64],
        now: DateTime<Utc>,
        at_hours: f64,
    ) -> HostLoadForecast {
        let deliveries: Vec<SimulatedDelivery<'_>> = running
            .iter()
            .map(|index| SimulatedDelivery {
                workspace_id: &self.evidence.workspace_id,
                task_id: &tasks[index].id,
                profile: &self.priced[index],
                age_minutes: age_minutes(admitted_at[index], at_hours),
            })
            .collect();
        simulated_forecast(
            now,
            &self.evidence.host,
            &deliveries,
            Arc::clone(&self.evidence.profiles),
            self.evidence.horizon_minutes,
        )
    }

    /// Whether a running delivery's profile still changes after `at_hours`:
    /// only then can the curve fall before a completion.
    pub(super) fn curve_changes(
        &self,
        running: &ForecastTaskSet,
        admitted_at: &[f64],
        at_hours: f64,
    ) -> bool {
        running.iter().any(|index| {
            self.priced[index].profile.as_ref().is_some_and(|profile| {
                (age_minutes(admitted_at[index], at_hours) as usize) + 1 < profile.series.len()
            })
        })
    }
}

/// One simulated pass's gap fit, built when the pass first reaches it.
pub(super) struct PassFit<'a> {
    fit: &'a ResourceFit,
    forecast: &'a std::cell::OnceCell<HostLoadForecast>,
    previous: Option<&'a GapFitHead>,
    gap_fit: Option<GapFit<'a>>,
    now: DateTime<Utc>,
}

impl<'a> PassFit<'a> {
    pub(super) fn new(
        fit: &'a ResourceFit,
        forecast: &'a std::cell::OnceCell<HostLoadForecast>,
        previous: Option<&'a GapFitHead>,
        forecast_at: DateTime<Utc>,
        at_hours: f64,
    ) -> Self {
        Self {
            fit,
            forecast,
            previous,
            gap_fit: None,
            now: forecast_at + Duration::milliseconds((at_hours * 3_600_000.0) as i64),
        }
    }

    /// Whether candidate `index` fits beside `running`, as gap fit judges it.
    pub(super) fn admits(
        &mut self,
        tasks: &[Task],
        index: usize,
        running: &ForecastTaskSet,
        admitted_at: &[f64],
        at_hours: f64,
    ) -> bool {
        let (fit, cell, previous, now) = (self.fit, self.forecast, self.previous, self.now);
        let gap_fit = self.gap_fit.get_or_insert_with(|| {
            let forecast =
                cell.get_or_init(|| fit.forecast(tasks, running, admitted_at, now, at_hours));
            GapFit::new(
                GapFitMode::Enforce,
                forecast,
                fit.evidence.admit_cpu_percent,
                fit.evidence.admit_memory_percent,
                &fit.evidence.workspace_id,
                HeadWait {
                    previous,
                    max_wait: Duration::minutes(i64::from(fit.evidence.head_max_wait_minutes)),
                    stuck_passes: fit.evidence.head_stuck_passes,
                    now,
                },
            )
        });
        gap_fit.admits_priced(&tasks[index], &fit.priced[index])
    }

    /// The head the next pass carries: this pass's, when it judged one, else
    /// the previous head while it is still `waiting`.
    pub(super) fn finish(self, waiting: impl FnOnce(&str) -> bool) -> Option<GapFitHead> {
        match self.gap_fit {
            // Every judged pass names its head, so `finish` never falls back
            // to the pending list.
            Some(gap_fit) => gap_fit.finish(&[]).0.head,
            None => self.previous.filter(|head| waiting(&head.task_id)).cloned(),
        }
    }
}

/// Whole minutes from `admitted_at` to `at_hours`.
fn age_minutes(admitted_at: f64, at_hours: f64) -> u32 {
    ((at_hours - admitted_at) * 60.0).max(0.0).floor() as u32
}
