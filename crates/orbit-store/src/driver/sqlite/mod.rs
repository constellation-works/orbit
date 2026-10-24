pub(crate) mod admission_load_store;
pub(crate) mod audit_event_store;
pub(crate) mod codebase_facts_store;
pub(crate) mod connection;
pub(crate) mod crew_performance_store;
pub(crate) mod invocation_store;
pub(crate) mod job_run_store;
pub mod migration;
pub(crate) mod plugin_store;
pub(crate) mod provider_limit_store;
pub(crate) mod read_pool;
pub(crate) mod reliability_store;
pub(crate) mod resource_usage_store;
pub(crate) mod retention;
pub(crate) mod routine_store;
pub(crate) mod task_commit_journal;
pub mod task_registry;
pub(crate) mod task_reservation_store;
pub(crate) mod tool_store;
pub(crate) mod v2_audit_store;

pub(crate) mod automation;
pub(crate) mod review;

pub(crate) mod friction_write;

// SQLite text ordering requires every writer to use the same timestamp encoding.
mod codec {
    use chrono::{DateTime, SecondsFormat, Utc};
    use orbit_common::OrbitError;
    use orbit_common::storage::sqlite::sqlite_store_error;
    use rusqlite::{Connection, Row, params};

    use crate::contracts::{HostResourceMinute, RunResourceMinute};

    /// Fixed-width UTC RFC3339 text (microseconds, `Z`) for lexical comparisons.
    pub(super) fn stamp(at: DateTime<Utc>) -> String {
        at.to_rfc3339_opts(SecondsFormat::Micros, true)
    }

    /// Clamp negative SQLite integers to zero.
    pub(super) fn unsigned(value: i64) -> u64 {
        u64::try_from(value).unwrap_or(0)
    }

    /// Column order consumed by `run_minute_from_row`.
    pub(super) const RUN_MINUTE_COLUMNS: &str =
        "workspace_id, run_id, minute_index, minute_started_at, task_id,
            work_class, complexity, resumed, delivery_admitted_at,
            cpu_cores, memory_bytes, samples, cpu_accounting";

    pub(super) fn run_minute_from_row(row: &Row<'_>) -> rusqlite::Result<RunResourceMinute> {
        Ok(RunResourceMinute {
            workspace_id: row.get(0)?,
            run_id: row.get(1)?,
            minute_index: row.get(2)?,
            minute_started_at: crate::parse_timestamp(&row.get::<_, String>(3)?)?,
            task_id: row.get(4)?,
            work_class: row.get(5)?,
            complexity: row.get(6)?,
            resumed: row.get(7)?,
            delivery_admitted_at: row
                .get::<_, Option<String>>(8)?
                .map(|at| crate::parse_timestamp(&at))
                .transpose()?,
            cpu_cores: row.get(9)?,
            memory_bytes: unsigned(row.get(10)?),
            samples: row.get(11)?,
            cpu_accounting: row.get(12)?,
        })
    }

    /// The host's minutes starting at or after `since`, oldest first.
    pub(super) fn host_minutes(
        conn: &Connection,
        since: DateTime<Utc>,
    ) -> Result<Vec<HostResourceMinute>, OrbitError> {
        let mut statement = conn
            .prepare(
                "SELECT minute_at, cpu_cores, cpu_count, memory_bytes, memory_total_bytes,
                        samples, recorded_by_run_id
                 FROM host_resource_minutes
                 WHERE minute_at >= ?1
                 ORDER BY minute_at",
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![stamp(since)], |row| {
                Ok(HostResourceMinute {
                    minute_at: crate::parse_timestamp(&row.get::<_, String>(0)?)?,
                    cpu_cores: row.get(1)?,
                    cpu_count: row.get(2)?,
                    memory_bytes: unsigned(row.get(3)?),
                    memory_total_bytes: unsigned(row.get(4)?),
                    samples: row.get(5)?,
                    recorded_by_run_id: row.get(6)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

#[cfg(test)]
mod tests;
