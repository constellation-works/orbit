//! The execution-host evidence resource fit simulates against [ORB-18689].
//!
//! Gap fit holds work below the leaf ceiling only where the drain enforces
//! it, so the forecast applies it only then: a local drain (or, with none
//! live, the host's configuration) in `enforce` mode, on a host whose size
//! it can read. It is captured once per forecast from this host, which is
//! where an owner-local drain runs:
//!
//! - **Size.** The core count and memory admission's thresholds are shares
//!   of, read as the live forecast reads them.
//! - **Baseline.** Load outside deliveries, from the newest admission pass
//!   recorded on this host within [`BASELINE_EVIDENCE_MINUTES`]; without one
//!   it counts as zero, as an unreadable baseline does in the live forecast.
//! - **Profiles.** The host's cached profile set, looked up per task through
//!   gap fit's own fallback chain.
//!
//! A pull drain's leaves are picked by the owner from the kinds of work its
//! request says fit, not by gap fit, so a pull ceiling reports
//! `pull_drain_not_modelled` and schedules by slots and locks alone. `shadow`
//! and `off` admit without gap fit, and so does the simulation.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::ResourceUsageStoreBackend as _;
use orbit_types::workflow::AutoConcurrencyMode;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::forecast::{ForecastHostEvidence, RESOURCE_RECHECK_HOURS};
use crate::runtime::host_resource::forecast::SimulatedHost;

/// How recent the admission pass the baseline is read from must be.
const BASELINE_EVIDENCE_MINUTES: i64 = 15;

/// What the forecast reports of resource fit, and the evidence it applies.
pub(super) struct ResourceFitInputs {
    pub(super) evidence: Option<ForecastHostEvidence>,
    pub(super) report: Value,
}

/// Resource fit for a drain admitting under `concurrency_source`'s ceiling.
pub(super) fn resource_fit_inputs(
    runtime: &OrbitRuntime,
    concurrency_source: &str,
    now: DateTime<Utc>,
) -> Result<ResourceFitInputs, OrbitError> {
    let settings = runtime.auto_concurrency_settings();
    let (mode, mode_source) = runtime.drain_auto_concurrency_mode()?;
    let mut report = json!({
        "status": mode.as_str(),
        "mode": mode.as_str(),
        "mode_source": mode_source,
        "admit_cpu_percent": settings.admit_cpu_percent,
        "admit_memory_percent": settings.admit_memory_percent,
        "head_max_wait_minutes": settings.head_max_wait_minutes,
        "head_stuck_passes": settings.head_stuck_passes,
        "horizon_cap_minutes": settings.horizon_cap_minutes,
        "recheck_minutes": (RESOURCE_RECHECK_HOURS * 60.0).round(),
        "host": null,
        "baseline": null,
        "profiles": null,
    });
    let unapplied = |report: Value| ResourceFitInputs {
        evidence: None,
        report,
    };
    if concurrency_source.starts_with("pull_") {
        report["status"] = json!("pull_drain_not_modelled");
        return Ok(unapplied(report));
    }
    if mode != AutoConcurrencyMode::Enforce {
        return Ok(unapplied(report));
    }
    let Some(capacity) = runtime
        .host_capacity()
        .filter(|host| host.cpu_count > 0 && host.memory_total_bytes > 0)
    else {
        report["status"] = json!("host_capacity_unknown");
        return Ok(unapplied(report));
    };
    report["host"] = json!({
        "cpu_count": capacity.cpu_count,
        "memory_total_bytes": capacity.memory_total_bytes,
        "source": "live",
    });
    let profiles = match runtime.resource_profiles() {
        Ok(profiles) => profiles,
        Err(error) => {
            tracing::warn!(%error, "backlog forecast: resource profiles unreadable");
            report["status"] = json!("profiles_unreadable");
            return Ok(unapplied(report));
        }
    };
    report["profiles"] = json!(profiles.profiles.len());
    let since = now - Duration::minutes(BASELINE_EVIDENCE_MINUTES);
    let recorded = runtime
        .sqlite_store()?
        .admission_forecasts(since)?
        .into_iter()
        .rev()
        .find(|record| {
            record.baseline_cpu_cores.is_some() || record.baseline_memory_bytes.is_some()
        });
    let host = SimulatedHost {
        capacity,
        baseline_cpu_cores: recorded
            .as_ref()
            .and_then(|record| record.baseline_cpu_cores),
        baseline_memory_bytes: recorded
            .as_ref()
            .and_then(|record| record.baseline_memory_bytes)
            .map(|bytes| bytes as f64),
    };
    report["baseline"] = match &recorded {
        Some(record) => json!({
            "source": "admission_forecast",
            "at": record.forecast_at.to_rfc3339(),
            "cpu_cores": host.baseline_cpu_cores,
            "memory_bytes": record.baseline_memory_bytes,
        }),
        None => json!({ "source": "none", "at": null, "cpu_cores": null, "memory_bytes": null }),
    };
    report["status"] = json!("applied");
    Ok(ResourceFitInputs {
        evidence: Some(ForecastHostEvidence {
            host,
            profiles,
            workspace_id: runtime.workspace_id()?,
            admit_cpu_percent: settings.admit_cpu_percent,
            admit_memory_percent: settings.admit_memory_percent,
            head_max_wait_minutes: settings.head_max_wait_minutes,
            head_stuck_passes: settings.head_stuck_passes,
            horizon_minutes: settings.horizon_cap_minutes,
        }),
        report,
    })
}
