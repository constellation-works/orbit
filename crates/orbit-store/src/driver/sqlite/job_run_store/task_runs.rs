//! Independently bounded per-task run pages, batched before hydration.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::JobRun;

use super::queries::{
    JOB_RUN_COLUMNS, job_run_order_sql, read_steps_for_runs, row_to_job_run, task_binding_sql,
};
use crate::Store;
use crate::contracts::JobRunOrder;

// Below both SQLite's compound-SELECT limit and its bound-parameter cap.
const TASK_ID_CHUNK: usize = 100;

impl Store {
    pub(super) fn recent_job_runs_for_tasks_for_workspace(
        &self,
        workspace_id: &str,
        job_id: &str,
        task_ids: &[String],
        limit_per_task: usize,
    ) -> Result<BTreeMap<String, Vec<JobRun>>, OrbitError> {
        let mut grouped: BTreeMap<String, Vec<JobRun>> = BTreeMap::new();
        if task_ids.is_empty() || limit_per_task == 0 {
            return Ok(grouped);
        }
        let ids = task_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let conn = self.read()?;
        for chunk in ids.chunks(TASK_ID_CHUNK) {
            // Each branch stops at its own LIMIT. Neither unrelated history
            // nor a busy task can displace another task's newest evidence.
            // UNION ALL preserves the task group for shared, multi-task runs.
            let order = job_run_order_sql(JobRunOrder::Recency);
            let pages = (0..chunk.len()).map(|index| {
                let placeholder = index + 4;
                let binding = task_binding_sql(placeholder);
                format!(
                    "SELECT * FROM (SELECT {JOB_RUN_COLUMNS}, ?{placeholder} AS requested_task_id \
                     FROM job_runs WHERE workspace_id = ?1 AND job_id = ?2 AND {binding} \
                     ORDER BY {order} LIMIT ?3)"
                )
            }).collect::<Vec<_>>().join(" UNION ALL ");
            let sql = format!("SELECT * FROM ({pages}) ORDER BY requested_task_id ASC, {order}");
            let limit = limit_per_task as i64;
            let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&workspace_id, &job_id, &limit];
            params.extend(chunk.iter().map(|id| *id as &dyn rusqlite::types::ToSql));
            let mut statement = conn.prepare(&sql).map_err(sqlite_store_error)?;
            let rows = statement
                .query_map(params.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>("requested_task_id")?,
                        row_to_job_run(row)?,
                    ))
                })
                .map_err(sqlite_store_error)?;
            let selected = rows
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?;
            drop(statement);
            let run_ids = selected
                .iter()
                .map(|(_, run)| run.run_id.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let steps = read_steps_for_runs(&conn, workspace_id, &run_ids)?;
            for (task_id, mut run) in selected {
                run.steps = steps.get(&run.run_id).cloned().unwrap_or_default();
                grouped.entry(task_id).or_default().push(run);
            }
        }
        Ok(grouped)
    }
}
