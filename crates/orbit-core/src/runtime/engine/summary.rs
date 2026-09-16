use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_store::scoreboard_summary::{
    ORCHESTRATION_SCHEMA_VERSION, OrchestrationSummary, ScoreboardInputs, ScoreboardWindow,
};
use orbit_types::workflow::JobRunState;

use crate::OrbitRuntime;

const RECENT_WINDOW_DAYS: i64 = 7;
/// Cap on the "most-called tools" leaderboard. 50 is comfortably above the
/// distinct (role, tool) pair count we observe in current workspaces and
/// keeps `summary.json` size bounded.
const TOP_TOOLS_LIMIT: usize = 50;

impl OrbitRuntime {
    /// Build a scoreboard summary for the workspace.
    ///
    /// `window`: `None` (or `Some(ScoreboardWindow::All)`) preserves the
    /// legacy lifetime view. A finite window scopes audit-sourced fields
    /// to the matching SQL cutoff and zeroes snapshot-sourced fields
    /// (see [`ScoreboardWindow`] for per-source semantics). `recent_7d`
    /// stays fixed at 7d regardless of `window`.
    pub fn generate_scoreboard_summary(
        &self,
        window: Option<ScoreboardWindow>,
    ) -> Result<orbit_store::scoreboard_summary::ScoreboardSummary, OrbitError> {
        let window = window.unwrap_or_default();
        let tasks = self.list_tasks()?;

        let now = Utc::now();
        let since_recent = now - Duration::days(RECENT_WINDOW_DAYS);
        let since_window = window.duration().map(|d| now - d);
        let orchestration = self.orchestrator_invocation_metrics(since_window, Some(now))?;
        let previous_normalized_tokens = match (since_window, window.duration()) {
            (Some(since), Some(duration)) => Some(
                self.orchestrator_invocation_metrics(Some(since - duration), Some(since))?
                    .normalized_tokens,
            ),
            _ => None,
        };

        let audit_tool_calls = self.audit_tool_call_counts_by_role(since_window.as_ref())?;
        let audit_tool_calls_by_surface =
            self.audit_tool_call_counts_by_surface_and_role(since_window.as_ref())?;
        let audit_tool_calls_by_surface_recent =
            self.audit_tool_call_counts_by_surface_and_role(Some(&since_recent))?;
        let top_tool_calls = self.audit_top_tool_calls(since_window.as_ref(), TOP_TOOLS_LIMIT)?;
        let job_runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            state: Some(JobRunState::Success),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        // Same cutoff `generate_summary_with_inputs` derives internally, applied
        // in SQL so the scoreboard never materializes the friction corpus
        // (ORB-10680).
        let friction_reported =
            crate::runtime::friction::store_for(self)?.reported_by_model(since_window)?;

        // Notable completions and coverage notes are projected inside
        // generate_summary_with_inputs from this same `tasks` slice — no extra
        // store read, and no model inference (ORB-10873).
        let summary = orbit_store::scoreboard_summary::generate_summary_with_inputs(
            &self.paths().scoreboard_dir,
            &tasks,
            &ScoreboardInputs {
                audit_tool_calls: &audit_tool_calls,
                audit_tool_calls_by_surface: &audit_tool_calls_by_surface,
                audit_tool_calls_by_surface_recent: &audit_tool_calls_by_surface_recent,
                job_runs: &job_runs,
                top_tool_calls: &top_tool_calls,
                friction_reported: &friction_reported,
                now: Some(now),
                window,
                orchestration: Some(OrchestrationSummary {
                    schema_version: ORCHESTRATION_SCHEMA_VERSION,
                    scope: "managed_execution".to_string(),
                    as_of: orchestration.as_of,
                    since: orchestration.since,
                    until: orchestration.until,
                    buckets: orchestration.buckets,
                    normalized_tokens: orchestration.normalized_tokens,
                    previous_normalized_tokens,
                }),
            },
        )?;
        let _ =
            orbit_store::scoreboard_summary::write_summary(&self.paths().scoreboard_dir, &summary)?;
        Ok(summary)
    }
}
