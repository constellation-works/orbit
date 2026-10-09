//! The dashboard's `orbit doctor` report: run on demand, bounded and cached.
//!
//! The report runs the same read-only probes as `orbit doctor`
//! ([`orbit_cmd::doctor_report_probes`]), never with `--deep` and never a
//! repair. Some probes take seconds (the state-directory permission walk
//! visits every directory under the Orbit roots), so the dashboard never runs
//! the report on its poll: a run happens when no recent report is cached, or
//! on an explicit refresh. Each probe runs on the blocking pool under its own
//! time bound, and the whole run under a budget below the forward's upstream
//! timeout, so a wedged probe costs one row, not the panel.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_cmd::{DoctorProbe, WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

/// Upper bound on one probe. The slowest probe on a large host, the
/// state-directory permission walk, takes several seconds.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on a whole run, kept below the 60 s bound the `/api/on/<host>`
/// forward puts on one request. Probes still queued when it is spent are
/// reported as not run.
pub(crate) const RUN_BUDGET: Duration = Duration::from_secs(45);

/// A plain read reuses a report this recent instead of running doctor again.
/// An explicit refresh always runs.
pub(crate) const REUSE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// What a request wants from the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DoctorRead {
    /// The cached report of any age, or none. Never runs doctor.
    Peek,
    /// A report from within [`REUSE_WINDOW`], running doctor when none is.
    Recent,
    /// A report from a run that started after this request arrived.
    Refresh,
}

/// Per-server cache of the last report for each workspace runtime.
pub(crate) struct DoctorReports {
    slots: Mutex<HashMap<usize, Arc<Slot>>>,
}

struct Slot {
    /// Serializes runs for one workspace, so overlapping refreshes from
    /// several tabs share one run. Never held while `slots` is locked.
    gate: tokio::sync::Mutex<()>,
    report: Mutex<Option<Cached>>,
}

struct Cached {
    runtime: Weak<OrbitRuntime>,
    started: Instant,
    finished: Instant,
    report: Arc<Report>,
}

/// One completed run.
pub(crate) struct Report {
    ran_at: DateTime<Utc>,
    duration_ms: u64,
    rows: Vec<WorkspaceDoctorResult>,
}

impl DoctorReports {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Answer `read` for `runtime`. `None` only for a [`DoctorRead::Peek`]
    /// with nothing cached.
    pub(crate) async fn read(
        &self,
        runtime: &Arc<OrbitRuntime>,
        read: DoctorRead,
    ) -> Option<(Arc<Report>, Instant)> {
        let requested = Instant::now();
        let slot = self.slot(runtime);
        let usable = |cached: &Cached| match read {
            DoctorRead::Peek => true,
            DoctorRead::Recent => cached.finished.elapsed() < REUSE_WINDOW,
            DoctorRead::Refresh => cached.started >= requested,
        };
        if let Some(hit) = slot.hit(runtime, usable) {
            return Some(hit);
        }
        if read == DoctorRead::Peek {
            return None;
        }
        let _gate = slot.gate.lock().await;
        // A run that finished while this request waited answers it too.
        if let Some(hit) = slot.hit(runtime, usable) {
            return Some(hit);
        }
        let started = Instant::now();
        let report = Arc::new(run_bounded(Arc::clone(runtime)).await);
        let finished = Instant::now();
        *slot.report.lock().unwrap_or_else(PoisonError::into_inner) = Some(Cached {
            runtime: Arc::downgrade(runtime),
            started,
            finished,
            report: Arc::clone(&report),
        });
        Some((report, finished))
    }

    fn slot(&self, runtime: &Arc<OrbitRuntime>) -> Arc<Slot> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        // A report for a runtime that has since been rebuilt or evicted can
        // never be served again; drop it unless a caller still holds the slot.
        slots.retain(|_, slot| Arc::strong_count(slot) > 1 || slot.live());
        Arc::clone(
            slots
                .entry(Arc::as_ptr(runtime) as usize)
                .or_insert_with(|| {
                    Arc::new(Slot {
                        gate: tokio::sync::Mutex::new(()),
                        report: Mutex::new(None),
                    })
                }),
        )
    }
}

impl Slot {
    fn hit(
        &self,
        runtime: &Arc<OrbitRuntime>,
        usable: impl Fn(&Cached) -> bool,
    ) -> Option<(Arc<Report>, Instant)> {
        let guard = self.report.lock().unwrap_or_else(PoisonError::into_inner);
        let cached = guard.as_ref().filter(|cached| usable(cached))?;
        let live = cached.runtime.upgrade()?;
        Arc::ptr_eq(&live, runtime).then(|| (Arc::clone(&cached.report), cached.finished))
    }

    fn live(&self) -> bool {
        let guard = self.report.lock().unwrap_or_else(PoisonError::into_inner);
        guard
            .as_ref()
            .is_none_or(|cached| cached.runtime.strong_count() > 0)
    }
}

impl Report {
    /// The `/api/doctor` body. `finished` is when the run completed, so the
    /// age is measured on this host's clock rather than the browser's.
    pub(crate) fn to_json(&self, finished: Instant) -> Value {
        let count = |status| self.rows.iter().filter(|row| row.status == status).count();
        json!({
            "ran_at": self.ran_at.to_rfc3339(),
            "age_ms": elapsed_ms(finished),
            "duration_ms": self.duration_ms,
            "failures": count(WorkspaceDoctorStatus::Error),
            "warnings": count(WorkspaceDoctorStatus::Warning),
            "check_timeout_ms": elapsed_ms_of(CHECK_TIMEOUT),
            "checks": self.rows.iter().map(orbit_cmd::doctor_row_json).collect::<Vec<_>>(),
        })
    }
}

/// Run every report probe in order, each under [`CHECK_TIMEOUT`] and all
/// under [`RUN_BUDGET`]. Always `deep = false`.
async fn run_bounded(runtime: Arc<OrbitRuntime>) -> Report {
    let ran_at = Utc::now();
    let started = Instant::now();
    let mut rows = Vec::new();
    for probe in orbit_cmd::doctor_report_probes() {
        let remaining = RUN_BUDGET.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            rows.push(not_run_row(probe));
            continue;
        }
        let bound = CHECK_TIMEOUT.min(remaining);
        let probe_runtime = Arc::clone(&runtime);
        let probe_copy = *probe;
        let run = tokio::task::spawn_blocking(move || probe_copy.run(&probe_runtime, false));
        match tokio::time::timeout(bound, run).await {
            Ok(Ok(probe_rows)) => rows.extend(probe_rows),
            Ok(Err(join_error)) => rows.push(unfinished_row(
                probe,
                WorkspaceDoctorStatus::Error,
                format!("the check panicked in the dashboard: {join_error}"),
                bound,
            )),
            // The blocking thread cannot be cancelled; it finishes on its own
            // and its result is discarded.
            Err(_) => rows.push(unfinished_row(
                probe,
                WorkspaceDoctorStatus::Warning,
                format!(
                    "the check did not finish within {} s, so its result is unknown",
                    bound.as_secs()
                ),
                bound,
            )),
        }
    }
    Report {
        ran_at,
        duration_ms: elapsed_ms(started),
        rows,
    }
}

fn unfinished_row(
    probe: &DoctorProbe,
    status: WorkspaceDoctorStatus,
    message: String,
    bound: Duration,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        check_name: probe.name.to_string(),
        status,
        message,
        remediation: Some("Run `orbit doctor` on this host to see this check.".to_string()),
        duration_ms: elapsed_ms_of(bound),
    }
}

fn not_run_row(probe: &DoctorProbe) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        check_name: probe.name.to_string(),
        status: WorkspaceDoctorStatus::Skipped,
        message: format!(
            "not run: earlier checks used the dashboard's {} s doctor budget",
            RUN_BUDGET.as_secs()
        ),
        remediation: Some("Run `orbit doctor` on this host to see this check.".to_string()),
        duration_ms: 0,
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    elapsed_ms_of(since.elapsed())
}

fn elapsed_ms_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
