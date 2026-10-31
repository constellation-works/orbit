//! Bounded, indexed per-task delivery reads, batched before hydration.
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::JobRun;

use super::queries::{JOB_RUN_COLUMNS, read_steps_for_runs, row_to_job_run};
use crate::Store;

// Under both SQLite's parameter limit and compound SELECT limit.
const TASK_CHUNK: usize = 100;

impl Store {
    pub(super) fn recent_job_runs_for_tasks_for_workspace(
        &self,
        workspace_id: &str,
        job_id: &str,
        task_ids: &[String],
        per_task_limit: usize,
    ) -> Result<BTreeMap<String, Vec<JobRun>>, OrbitError> {
        let mut result: BTreeMap<String, Vec<JobRun>> = BTreeMap::new();
        if per_task_limit == 0 || task_ids.is_empty() {
            return Ok(result);
        }
        let mut ids = task_ids.iter().collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        let conn = self.read()?;
        for chunk in ids.chunks(TASK_CHUNK) {
            // Each branch seeks one ownership prefix and stops at its own
            // limit. Joining only the selected run IDs keeps JSON and steps
            // outside the history walk. Neither retries nor unrelated tasks
            // can consume another task's window.
            let pages = (0..chunk.len())
                .map(|index| {
                    format!(
                        "SELECT * FROM (SELECT task_id, run_id FROM job_run_tasks
                 WHERE workspace_id = ?1 AND job_id = ?2 AND task_id = ?{}
                 ORDER BY recency DESC, run_id ASC LIMIT ?3)",
                        index + 4
                    )
                })
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            let sql = format!(
                "SELECT job_runs.{JOB_RUN_COLUMNS}, requested.task_id FROM ({pages}) AS requested
                 CROSS JOIN job_runs WHERE job_runs.workspace_id = ?1
                 AND job_runs.run_id = requested.run_id"
            );
            let limit = i64::try_from(per_task_limit).unwrap_or(i64::MAX);
            let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&workspace_id, &job_id, &limit];
            params.extend(chunk.iter().map(|id| *id as &dyn rusqlite::types::ToSql));
            let mut statement = conn.prepare(&sql).map_err(sqlite_store_error)?;
            let rows = statement
                .query_map(params.as_slice(), |row| {
                    Ok((row.get::<_, String>(17)?, row_to_job_run(row)?))
                })
                .map_err(sqlite_store_error)?;
            for row in rows {
                let (task_id, run) = row.map_err(sqlite_store_error)?;
                result.entry(task_id).or_default().push(run);
            }
        }
        let mut run_ids = result
            .values()
            .flatten()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        run_ids.sort();
        run_ids.dedup();
        let steps = read_steps_for_runs(&conn, workspace_id, &run_ids)?;
        for runs in result.values_mut() {
            runs.sort_by(|a, b| {
                b.finished_at
                    .or(b.started_at)
                    .unwrap_or(b.created_at)
                    .cmp(&a.finished_at.or(a.started_at).unwrap_or(a.created_at))
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            for run in runs {
                run.steps = steps.get(&run.run_id).cloned().unwrap_or_default();
            }
        }
        Ok(result)
    }
}
