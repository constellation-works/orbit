//! Gap-fit admission for a local drain pass (`machine.auto_concurrency`).
//!
//! `select_admissions` asks [`GapFit::admits`] about each candidate that
//! passed every other check: the ceiling, the footprint guard and conflicts.
//! Gap fit only ever removes candidates:
//!
//! - **Fit.** The candidate's profile, added to the host load forecast, must
//!   stay under `admit_cpu_percent` of the host's cores and
//!   `admit_memory_percent` of its memory at every minute of its horizon. A
//!   candidate that fits is added to the forecast before the next is checked,
//!   so one pass can admit several that account for each other. One that
//!   does not is recorded with the minute and resource that failed, and the
//!   walk goes on to smaller work.
//! - **Starvation guard.** The first candidate gap fit judges in a pass is
//!   the head. Once it has failed for longer than `head_max_wait_minutes`,
//!   over consecutive passes, nothing behind it is admitted until it fits.
//! - **No profiles.** On a host without profiles every lookup finds none
//!   and every candidate fits, so admission is what it is without gap fit.
//!   A host whose size cannot be read admits the same way.
//!
//! In `shadow` the drain admits as `off` does and keeps this only as the
//! record of what `enforce` would have done; in `enforce` it decides.

use chrono::{DateTime, Duration, Utc};
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use orbit_types::workflow::{
    DrainAdmissionForecast, ForecastPeak, GapFitAdmission, GapFitHead, GapFitSkip,
};

use crate::runtime::host_resource::delivery::work_class;
use crate::runtime::host_resource::forecast::{
    AdmissionReservationRequest, ForecastGap, HostLoadForecast,
};
use crate::runtime::host_resource::profile::ProfileLookup;

/// `machine.auto_concurrency.mode` when it is not `off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::adapter::engine_host::v2_host) enum GapFitMode {
    /// Decide and record, but admit as `off`.
    Shadow,
    /// Admit by gap fit.
    Enforce,
}

impl GapFitMode {
    /// The mode a setting names; `None` for `off`.
    pub(in crate::adapter::engine_host::v2_host) fn from_setting(mode: &str) -> Option<Self> {
        match mode {
            "shadow" => Some(Self::Shadow),
            "enforce" => Some(Self::Enforce),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Enforce => "enforce",
        }
    }
}

/// The starvation guard's memory between passes.
#[derive(Clone, Copy, Debug)]
pub(in crate::adapter::engine_host::v2_host) struct HeadWait<'a> {
    /// The previous pass's head, while it was failing.
    pub(in crate::adapter::engine_host::v2_host) previous: Option<&'a GapFitHead>,
    pub(in crate::adapter::engine_host::v2_host) max_wait: Duration,
    pub(in crate::adapter::engine_host::v2_host) now: DateTime<Utc>,
}

impl HeadWait<'_> {
    fn head(&self, task_id: &str) -> GapFitHead {
        let failing_since = self
            .previous
            .filter(|head| head.task_id == task_id)
            .map_or(self.now, |head| head.failing_since);
        GapFitHead {
            task_id: task_id.to_string(),
            failing_since,
            protected: self.now - failing_since > self.max_wait,
        }
    }

    /// The previous head, while it is still waiting in `pending`: a pass
    /// that judged no head breaks no wait.
    fn carried(&self, pending: &[String]) -> Option<GapFitHead> {
        self.previous
            .filter(|head| pending.contains(&head.task_id))
            .map(|head| self.head(&head.task_id))
    }

    /// The record of a pass that built no forecast: no slot was free, or no
    /// candidate was left to fit.
    pub(in crate::adapter::engine_host::v2_host) fn record_without_forecast(
        &self,
        mode: GapFitMode,
        pending: &[String],
    ) -> DrainAdmissionForecast {
        DrainAdmissionForecast {
            mode: mode.as_str().to_string(),
            forecast_at: None,
            cpu_peak: None,
            memory_peak: None,
            admitted: Vec::new(),
            skipped: Vec::new(),
            head: self.carried(pending),
        }
    }
}

/// The profile lookup that prices `task` in `workspace_id`: its work class
/// from its tags now, its complexity, and `no-diff-expected` for a
/// template's base class.
pub(in crate::adapter::engine_host::v2_host) fn task_lookup(
    workspace_id: &str,
    task: &Task,
) -> ProfileLookup {
    ProfileLookup::new(
        workspace_id,
        work_class(&task.tags),
        task.complexity.map(|complexity| complexity.to_string()),
        task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG),
    )
}

/// One pass's gap fit against one forecast.
pub(in crate::adapter::engine_host::v2_host) struct GapFit<'a> {
    mode: GapFitMode,
    forecast: &'a HostLoadForecast,
    gap: Option<ForecastGap>,
    workspace_id: &'a str,
    head_wait: HeadWait<'a>,
    /// Whether this pass has judged its head.
    head_judged: bool,
    head: Option<GapFitHead>,
    admitted: Vec<GapFitAdmission>,
    skipped: Vec<GapFitSkip>,
    reservations: Vec<AdmissionReservationRequest>,
}

impl<'a> GapFit<'a> {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        mode: GapFitMode,
        forecast: &'a HostLoadForecast,
        admit_cpu_percent: u8,
        admit_memory_percent: u8,
        workspace_id: &'a str,
        head_wait: HeadWait<'a>,
    ) -> Self {
        Self {
            mode,
            forecast,
            gap: forecast.gap(admit_cpu_percent, admit_memory_percent),
            workspace_id,
            head_wait,
            head_judged: false,
            head: None,
            admitted: Vec::new(),
            skipped: Vec::new(),
            reservations: Vec::new(),
        }
    }

    /// Whether `task` fits the gap. One that fits is counted in the forecast
    /// from now on; one that does not, or that the starvation guard holds,
    /// is recorded with why.
    pub(in crate::adapter::engine_host::v2_host) fn admits(&mut self, task: &Task) -> bool {
        let lookup = task_lookup(self.workspace_id, task);
        let found = self.forecast.profiles.lookup(&lookup);
        let profile_source = found.source.as_str().to_string();
        if let Some(head) = self.head.as_ref().filter(|head| head.protected) {
            self.skipped.push(GapFitSkip {
                task_id: task.id.clone(),
                profile_source,
                resource: None,
                minute: None,
                percent: None,
                detail: format!(
                    "waits behind {}, which has not fit since {}",
                    head.task_id,
                    head.failing_since.format("%Y-%m-%d %H:%MZ"),
                ),
            });
            return false;
        }
        let miss = match (&self.gap, found.profile.as_deref()) {
            (Some(gap), Some(profile)) => gap.fit(profile).err(),
            _ => None,
        };
        let is_head = !self.head_judged;
        self.head_judged = true;
        if let Some(miss) = miss {
            if is_head {
                self.head = Some(self.head_wait.head(&task.id));
            }
            self.skipped.push(GapFitSkip {
                task_id: task.id.clone(),
                profile_source,
                resource: Some(miss.resource.as_str().to_string()),
                minute: Some(miss.minute),
                percent: Some(miss.percent),
                detail: miss.describe(),
            });
            return false;
        }
        if let (Some(gap), Some(profile)) = (self.gap.as_mut(), found.profile.as_deref()) {
            gap.add(profile);
        }
        self.admitted.push(GapFitAdmission {
            task_id: task.id.clone(),
            profile_source,
        });
        self.reservations.push(AdmissionReservationRequest {
            task_id: task.id.clone(),
            lookup,
        });
        true
    }

    /// The pass's record, and the reservations for what it admitted.
    pub(in crate::adapter::engine_host::v2_host) fn finish(
        self,
        pending: &[String],
    ) -> (DrainAdmissionForecast, Vec<AdmissionReservationRequest>) {
        let head = if self.head_judged {
            self.head
        } else {
            self.head_wait.carried(pending)
        };
        let peak = |minute, value, percent: Option<f64>| ForecastPeak {
            minute,
            value,
            percent,
        };
        let cpu = &self.forecast.cpu.peak;
        let memory = &self.forecast.memory.peak;
        let record = DrainAdmissionForecast {
            mode: self.mode.as_str().to_string(),
            forecast_at: Some(self.forecast.at),
            cpu_peak: Some(peak(
                cpu.minute,
                cpu.value,
                self.gap.as_ref().map(|gap| gap.cpu_percent(cpu.value)),
            )),
            memory_peak: Some(peak(
                memory.minute,
                memory.value,
                self.gap
                    .as_ref()
                    .map(|gap| gap.memory_percent(memory.value)),
            )),
            admitted: self.admitted,
            skipped: self.skipped,
            head,
        };
        (record, self.reservations)
    }
}
