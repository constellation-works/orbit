//! SQL builders for run listings: the run-row select, its filter, ordering and index pins.

use orbit_types::workflow::JobRunState;

use crate::contracts::{JobRunOrder, JobRunQuery};

/// The run-row `SELECT` and its bound parameters for a [`JobRunQuery`]:
/// filter, order, and `LIMIT`. Steps are read separately.
pub(in crate::driver::sqlite::job_run_store) fn job_run_list_sql(
    workspace_id: &str,
    query: &JobRunQuery,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let (where_clause, mut params) = job_run_filter_sql(workspace_id, query);
    let order_clause = job_run_order_sql(query.order_by);
    if query.order_by == JobRunOrder::Recency
        && query.job_id.is_some()
        && names_success(query)
        && let Some(limit) = query.limit
    {
        // A page that admits successes walks a success history that can be
        // most of the workspace, so the state index would visit every job's
        // successes. Seek this job instead, sort narrow IDs before hydrating
        // the bounded page, and pin the job index because SQLite has no
        // selectivity statistics. Other pages keep their existing selection:
        // a job-prefix walk over them would scale with that job's success
        // history [ORB-22917].
        params.push(Box::new(limit as i64));
        return (
            format!(
                "SELECT {JOB_RUN_COLUMNS} FROM (SELECT run_id AS selected_run_id, \
                 COALESCE(finished_at, started_at, created_at) AS selected_recency \
                 FROM job_runs INDEXED BY idx_job_runs_ws_job_created \
                 WHERE {where_clause} ORDER BY {order_clause} LIMIT ?{}) AS selected \
                 CROSS JOIN job_runs WHERE workspace_id = ?1 \
                 AND run_id = selected.selected_run_id \
                 ORDER BY selected.selected_recency DESC, run_id ASC",
                params.len()
            ),
            params,
        );
    }
    if query.order_by == JobRunOrder::Recency
        && query.states.len() > 1
        && let Some(limit) = query.limit
    {
        // An IN predicate lets SQLite walk workspace recency and discard
        // unrelated states until LIMIT. Bound each distinct state first,
        // using its recency index even when other filters are present. The
        // global top N must be among the top N of these disjoint sets.
        let mut states = query
            .states
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        states.sort();
        states.dedup();
        params.push(Box::new(limit as i64));
        let limit_param = params.len();
        let pages = states
            .into_iter()
            .map(|state| {
                params.push(Box::new(state));
                format!(
                    "SELECT * FROM (SELECT {JOB_RUN_COLUMNS} FROM job_runs \
                 INDEXED BY idx_job_runs_ws_state_recency \
                 WHERE {where_clause} AND state = ?{} \
                 ORDER BY {order_clause} LIMIT ?{limit_param})",
                    params.len()
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        return (
            format!("SELECT * FROM ({pages}) ORDER BY {order_clause} LIMIT ?{limit_param}"),
            params,
        );
    }
    // A bounded recency page for one state must walk that state's recency
    // index and stop at its limit. Left to the planner, a `created_since`
    // window instead ranges over `idx_job_runs_workspace_created` and sorts
    // the whole retained cohort, payloads included, before the limit
    // applies [ORB-20163]: the store collects no planner statistics, so the
    // window's selectivity is only a heuristic guess. Pin the index the
    // multi-state path above already pins, but only when no other filter
    // could make the walk discard most of the state's history before
    // filling the page: a per-job, per-task or per-routine page is bounded
    // far more cheaply by its own index, and a terminal/active set that
    // contradicts the one state would walk the state to exhaustion.
    let single_state = match (query.state, query.states.as_slice()) {
        (Some(_), []) => true,
        (Some(state), [only]) => state == *only,
        (None, [_]) => true,
        _ => false,
    };
    let indexed_by = if query.order_by == JobRunOrder::Recency
        && query.limit.is_some()
        && single_state
        && query.job_id.is_none()
        && query.task_id.is_none()
        && query.trigger_routine.is_none()
        && !query.terminal_only
        && !query.active_only
    {
        " INDEXED BY idx_job_runs_ws_state_recency"
    } else if live_states_only(query) {
        // The live states are a handful of rows in any workspace, so their
        // state index answers exactly and the sort covers only them. Left to
        // the planner, `ORDER BY created_at` instead walks the workspace's
        // whole history through `idx_job_runs_workspace_created` and drops
        // every finished run: ~18 ms per call on a 41k-run workspace, paid
        // three times by each task detail's active-triage scan [ORB-21825].
        " INDEXED BY idx_job_runs_ws_state"
    } else {
        ""
    };
    let mut sql = format!(
        "SELECT {JOB_RUN_COLUMNS} FROM job_runs{indexed_by} \
         WHERE {where_clause} ORDER BY {order_clause}"
    );
    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
        params.push(Box::new(limit as i64));
    }
    (sql, params)
}

/// Whether `query`'s state predicates admit only live runs (`pending`,
/// `running`, `retrying`). A finished state, including `skipped`, can hold
/// most of a workspace's history, where the state index is no cheaper than
/// the created-at walk.
fn live_states_only(query: &JobRunQuery) -> bool {
    let live = |state: &JobRunState| {
        matches!(
            state,
            JobRunState::Pending | JobRunState::Running | JobRunState::Retrying
        )
    };
    let constrained = query.state.is_some() || !query.states.is_empty() || query.active_only;
    constrained
        && !query.terminal_only
        && query.state.as_ref().is_none_or(live)
        && query.states.iter().all(live)
}

/// Whether `query` names finished successes in either state filter, as a
/// failure-streak read does. Only such pages read success history.
fn names_success(query: &JobRunQuery) -> bool {
    query.state == Some(JobRunState::Success) || query.states.contains(&JobRunState::Success)
}

/// Columns [`row_to_job_run`] reads, in its order.
///
/// Every listing, show and filter path selects these, so none may sit behind
/// an unbounded payload in the stored record: SQLite reaches a column only by
/// walking the overflow-page chain of every large value stored before it.
/// Pipeline state lives in `job_run_states` for that reason.
pub(in crate::driver::sqlite::job_run_store) const JOB_RUN_COLUMNS: &str = "run_id, job_id, attempt, state, scheduled_at, \
     started_at, finished_at, duration_ms, created_at, pid, pid_start_time, input_json, \
     retry_source_run_id, knowledge_metrics_json, resolved_crew, \
     COALESCE(crew_model, implementer_model), executed_on_json, build_json";

/// Probe the existing per-job created index once per requested ID, then
/// hydrate only that run. A window over all history would scan old runs to
/// answer a catalog refresh; the correlated LIMIT stops at its first match.
pub(super) fn latest_job_runs_sql(job_count: usize) -> String {
    let values = (2..job_count + 2)
        .map(|index| format!("(?{index})"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "WITH requested(requested_id) AS (VALUES {values}) \
         SELECT {JOB_RUN_COLUMNS} FROM requested CROSS JOIN job_runs \
         WHERE workspace_id = ?1 AND run_id = (\
             SELECT run_id FROM job_runs WHERE workspace_id = ?1 \
             AND job_id = requested.requested_id \
             ORDER BY created_at DESC, run_id ASC LIMIT 1)"
    )
}

/// `WHERE` clause and bound parameters for a [`JobRunQuery`] on `job_runs`,
/// shared by the list, count, and duration reads so the three cannot drift.
pub(super) fn job_run_filter_sql(
    workspace_id: &str,
    query: &JobRunQuery,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec!["workspace_id = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(workspace_id.to_string())];
    if let Some(job_id) = &query.job_id {
        conditions.push(format!("job_id = ?{}", params.len() + 1));
        params.push(Box::new(job_id.clone()));
    }
    if let Some(task_id) = &query.task_id {
        // Ownership follows the same bindings the TASK column shows: a text
        // element of the top-level `task_ids` array, or a text top-level
        // `task_id`. `job_run_tasks` (v59) materializes exactly those
        // bindings on every write, so the filter reads it instead of
        // parsing every run's input: a per-task JSON walk over the whole
        // workspace history cost ~90 ms per call on a 36k-run store, and
        // the experiment readout makes two per enrolled task [ORB-20412].
        let placeholder = params.len() + 1;
        conditions.push(format!(
            "run_id IN (SELECT run_id FROM job_run_tasks \
             WHERE workspace_id = ?1 AND task_id = ?{placeholder})"
        ));
        params.push(Box::new(task_id.clone()));
    }
    if let Some(routine) = &query.trigger_routine {
        // Pre-filter on the run's own input before probing `job_run_states`:
        // parsing each candidate's pipeline state means walking its overflow
        // chain (hundreds of KB on drain runs) only to read one trigger
        // field. Every term below only excludes a run whose input proves it
        // was not admitted for this routine, so the state probe still decides
        // every kept row: a state or delivery automation run names its
        // consumer (`<machine>/<workspace>/<kind>/<routine>`) in the input it
        // was submitted with, and drain-promoted or CI-sweep-filed child runs
        // carry their authority records in theirs and are never
        // routine-triggered.
        let consumer_pattern = format!(
            "%/{}",
            routine
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let pattern = params.len() + 1;
        conditions.push(format!(
            "(COALESCE(json_extract(input_json, '$.state_automation.consumer') \
             LIKE ?{pattern} ESCAPE '\\', 1) \
             AND COALESCE(json_extract(input_json, '$.automation.batch.consumer') \
             LIKE ?{pattern} ESCAPE '\\', 1) \
             AND json_type(input_json, '$.drain_promotion') IS NULL \
             AND json_type(input_json, '$.ci_sweep_filing') IS NULL)"
        ));
        params.push(Box::new(consumer_pattern));
        conditions.push(format!(
            "EXISTS (SELECT 1 FROM job_run_states AS trigger_state \
             WHERE trigger_state.workspace_id = job_runs.workspace_id \
             AND trigger_state.run_id = job_runs.run_id \
             AND json_extract(trigger_state.pipeline_state_json, '$.trigger.routine') = ?{})",
            params.len() + 1
        ));
        params.push(Box::new(routine.clone()));
    }
    if let Some(state) = query.state {
        conditions.push(format!("state = ?{}", params.len() + 1));
        params.push(Box::new(state.to_string()));
    }
    if !query.states.is_empty() {
        let placeholders = query
            .states
            .iter()
            .map(|state| {
                params.push(Box::new(state.to_string()));
                format!("?{}", params.len())
            })
            .collect::<Vec<_>>()
            .join(", ");
        conditions.push(format!("state IN ({placeholders})"));
    }
    if query.terminal_only {
        conditions.push(
            "state IN ('success', 'failed', 'timeout', 'skipped', 'cancelled', 'interrupted', 'held')"
                .to_string(),
        );
    }
    if query.active_only {
        conditions.push("state IN ('pending', 'running')".to_string());
    }
    if let Some(created_since) = query.created_since {
        conditions.push(format!("created_at >= ?{}", params.len() + 1));
        params.push(Box::new(created_since.to_rfc3339()));
    }
    if let Some(recency_since) = query.recency_since {
        conditions.push(format!(
            "COALESCE(finished_at, started_at, created_at) >= ?{}",
            params.len() + 1
        ));
        params.push(Box::new(recency_since.to_rfc3339()));
    }
    (conditions.join(" AND "), params)
}

/// `ORDER BY` clause for a bounded [`JobRunQuery`], matched to
/// [`JobRunOrder`]. `run_id ASC` breaks ties deterministically in both
/// variants; timestamps are stored as fixed-width RFC 3339 text, so a
/// lexical `DESC` sort matches chronological order. `CreatedAt` is covered by
/// `idx_job_runs_workspace_created`. `Recency` [ORB-11251] is covered by the
/// v39 expression indexes `idx_job_runs_ws_recency` and, under a `state`
/// filter, `idx_job_runs_ws_state_recency`; SQLite uses an expression index
/// only when the `ORDER BY` spells the indexed expression identically, so
/// keep this text in step with the migration.
fn job_run_order_sql(order_by: JobRunOrder) -> &'static str {
    match order_by {
        JobRunOrder::CreatedAt => "created_at DESC, run_id ASC",
        JobRunOrder::Recency => "COALESCE(finished_at, started_at, created_at) DESC, run_id ASC",
    }
}
