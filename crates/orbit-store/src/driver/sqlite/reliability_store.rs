//! Count-only reliability reads over `job_runs` and `invocations` [ORB-10588].
//!
//! These queries exist so pipeline-reliability reporting never has to load the
//! wide row shapes that the token/cost metrics path uses. Every column read
//! here is an identifier, a state, or a timestamp: no token column and no cost
//! column is referenced, projected, or aggregated. That is deliberate — the
//! worker run store and `invocations` disagree on token accounting by an order
//! of magnitude (friction F2026-08-031), so any rate derived from those fields
//! would be confidently wrong. Durations and counts do not share that defect.
//!
//! Both queries are workspace-scoped. `job_runs` and `invocations` both carry
//! `workspace_id`; run ids are only unique within a workspace, so an
//! invocation counts only when its own workspace matches and its run, joined
//! on `(workspace_id, run_id)`, is present in `job_runs`. A legacy invocation
//! recorded without a workspace, or one whose run is absent, is excluded
//! rather than attributed to an arbitrary workspace.
//!
//! Windows are half-open (`since <= ts < until`) and compared as RFC3339 text,
//! matching every other timestamp filter in this crate (see
//! `list_job_runs_for_workspace` and `list_invocation_records`).

use chrono::{DateTime, Utc};
use rusqlite::params;

use orbit_common::OrbitError;

use crate::Store;

use crate::contracts::{
    ActivityInvocationCount, BoundedFacts, InvocationRunCoverage, JobRunOutcomeFact,
};

impl Store {
    /// Lists every job run created in `[since, until)` for one workspace,
    /// projected to `(job_id, state, created_at)`.
    ///
    /// Unlike [`Store::list_job_runs_for_workspace`], this does not read step
    /// rows, input payloads, or model attribution — a reliability window can
    /// span thousands of runs and the per-run step fan-out would dominate.
    ///
    /// Rows are ordered newest-first and capped at `max_rows`; when the cap
    /// binds, the returned [`BoundedFacts::truncated`] flag is set.
    pub fn list_job_run_outcome_facts(
        &self,
        workspace_id: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        max_rows: usize,
    ) -> Result<BoundedFacts<JobRunOutcomeFact>, OrbitError> {
        if max_rows == 0 {
            return Ok(BoundedFacts {
                facts: Vec::new(),
                truncated: false,
            });
        }
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT job_id, state, created_at
                FROM job_runs
                WHERE workspace_id = ?1 AND created_at >= ?2 AND created_at < ?3
                ORDER BY created_at DESC, run_id ASC
                LIMIT ?4
                "#,
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        // Over-fetch by one so a full page can be distinguished from a page
        // that happens to end exactly on the cap.
        let probe = max_rows.saturating_add(1) as i64;
        let rows = stmt
            .query_map(
                params![workspace_id, since.to_rfc3339(), until.to_rfc3339(), probe,],
                |row| {
                    Ok(JobRunOutcomeFact {
                        job_id: row.get(0)?,
                        state: row.get(1)?,
                        created_at: crate::parse_timestamp(&row.get::<_, String>(2)?)?,
                    })
                },
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let mut facts = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let truncated = facts.len() > max_rows;
        facts.truncate(max_rows);
        Ok(BoundedFacts { facts, truncated })
    }

    /// Counts invocations per `activity_id` in `[since, until)`, scoped to the
    /// runs owned by one workspace.
    ///
    /// Grouping happens in SQLite, so the result set is bounded by the number
    /// of distinct activities rather than by invocation volume.
    pub fn count_invocations_by_activity(
        &self,
        workspace_id: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Result<Vec<ActivityInvocationCount>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT i.activity_id,
                       COUNT(*) AS invocation_count,
                       COUNT(DISTINCT i.job_run_id) AS job_run_count
                FROM invocations i
                INNER JOIN job_runs jr
                        ON jr.workspace_id = i.workspace_id AND jr.run_id = i.job_run_id
                WHERE i.workspace_id = ?1 AND i.ts >= ?2 AND i.ts < ?3
                GROUP BY i.activity_id
                ORDER BY invocation_count DESC, i.activity_id ASC
                "#,
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(
                params![workspace_id, since.to_rfc3339(), until.to_rfc3339()],
                |row| {
                    Ok(ActivityInvocationCount {
                        activity_id: row.get(0)?,
                        invocation_count: row.get::<_, i64>(1)?.max(0) as u64,
                        job_run_count: row.get::<_, i64>(2)?.max(0) as u64,
                    })
                },
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Counts distinct job runs with any invocation in `[since, until)`, and
    /// the subset of those runs that invoked one of `activity_ids`.
    ///
    /// An empty `activity_ids` yields a zero `matching_job_runs` alongside a
    /// real `total_job_runs`, so a caller with nothing to match still gets a
    /// usable denominator.
    pub fn count_invocation_job_runs(
        &self,
        workspace_id: &str,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
        activity_ids: &[String],
    ) -> Result<InvocationRunCoverage, OrbitError> {
        let mut bound: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
            Box::new(workspace_id.to_string()),
            Box::new(since.to_rfc3339()),
            Box::new(until.to_rfc3339()),
        ];
        // `WHEN 0` is a constant-false guard for the empty case; SQLite has no
        // legal `IN ()` form.
        let matching_predicate = if activity_ids.is_empty() {
            "0".to_string()
        } else {
            let placeholders = activity_ids
                .iter()
                .enumerate()
                .map(|(offset, _)| format!("?{}", bound.len() + offset + 1))
                .collect::<Vec<_>>()
                .join(", ");
            for activity_id in activity_ids {
                bound.push(Box::new(activity_id.clone()));
            }
            format!("i.activity_id IN ({placeholders})")
        };

        let sql = format!(
            r#"
            SELECT COUNT(DISTINCT i.job_run_id),
                   COUNT(DISTINCT CASE WHEN {matching_predicate} THEN i.job_run_id END)
            FROM invocations i
            INNER JOIN job_runs jr
                    ON jr.workspace_id = i.workspace_id AND jr.run_id = i.job_run_id
            WHERE i.workspace_id = ?1 AND i.ts >= ?2 AND i.ts < ?3
            "#
        );

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            bound.iter().map(|value| value.as_ref()).collect();
        let conn = self.read()?;
        conn.query_row(&sql, param_refs.as_slice(), |row| {
            Ok(InvocationRunCoverage {
                total_job_runs: row.get::<_, i64>(0)?.max(0) as u64,
                matching_job_runs: row.get::<_, i64>(1)?.max(0) as u64,
            })
        })
        .map_err(|e| OrbitError::Store(e.to_string()))
    }
}
