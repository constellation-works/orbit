//! The forecast accuracy report with each selected drain's limiter shares.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;

use super::drains::DRAIN_JOB_NAME;
use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;
use crate::runtime::host_resource::accuracy::{AccuracySelection, DrainLimiters, ForecastAccuracy};

impl OrbitRuntime {
    /// How the admission forecasts recorded on this host over the last
    /// 30 days compare with its measured minutes, how each drain's shadow and
    /// enforce decisions compare with what it admitted, and what limited each
    /// drain's admission passes.
    pub fn forecast_accuracy(&self) -> Result<ForecastAccuracy, OrbitError> {
        let report_at = Utc::now();
        self.forecast_accuracy_selected(AccuracySelection::last_30_days(report_at), report_at)
    }

    /// The report over a selected cohort of recorded forecasts, bounded above
    /// by `report_at`, with the limiter shares of the drains it selects:
    /// every drain with a selected forecast and, unless a mode is selected,
    /// every drain of this workspace live at or after `since` that matches
    /// `drain`. A drain that has counted no pass is left out. Read-only.
    pub fn forecast_accuracy_selected(
        &self,
        selection: AccuracySelection,
        report_at: DateTime<Utc>,
    ) -> Result<ForecastAccuracy, OrbitError> {
        let mut report = self.forecast_accuracy_measured(selection, report_at)?;
        let selection = &report.selection;
        let mut drains: BTreeSet<(String, String)> = report
            .drains
            .iter()
            .map(|drain| (drain.workspace_id.clone(), drain.run_id.clone()))
            .collect();
        if selection.mode.is_none() {
            let workspace_id = self.workspace_id()?;
            for job_id in [DRAIN_JOB_NAME, PULL_DRAIN_JOB] {
                let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                    job_id: Some(job_id.to_string()),
                    include_steps: false,
                    ..Default::default()
                })?;
                drains.extend(
                    runs.into_iter()
                        .filter(|run| {
                            run.created_at <= report.report_at
                                && run
                                    .finished_at
                                    .is_none_or(|finished| finished >= selection.since)
                                && selection.drain.as_ref().is_none_or(|id| id == &run.run_id)
                        })
                        .map(|run| (workspace_id.clone(), run.run_id)),
                );
            }
        }
        let store = self.sqlite_store()?;
        let mut limiters = Vec::new();
        for (workspace_id, run_id) in drains {
            let counters = store
                .read_job_run_state_for_workspace(&workspace_id, &run_id)?
                .and_then(|state| state.pull_drain.drain_pass_counters);
            if let Some(counters) = counters {
                limiters.push(DrainLimiters::new(workspace_id, run_id, counters));
            }
        }
        report.limiters = limiters;
        Ok(report)
    }
}
