//! The per-workspace crew-performance snapshot in
//! `crew_performance_snapshot` / `crew_performance_cells`.
//!
//! Timestamps are stored as fixed-width UTC RFC3339 text (microseconds,
//! `Z`), as in `provider_limit_store`.

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{CrewPerformanceCell, CrewPerformanceSnapshot, CrewPerformanceStoreBackend};

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Micros, true)
}

impl CrewPerformanceStoreBackend for Store {
    fn replace_crew_performance_snapshot(
        &self,
        snapshot: &CrewPerformanceSnapshot,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            conn.execute(
                "DELETE FROM crew_performance_cells WHERE workspace_id = ?1",
                params![snapshot.workspace_id],
            )
            .map_err(sqlite_store_error)?;
            conn.execute(
                "DELETE FROM crew_performance_snapshot WHERE workspace_id = ?1",
                params![snapshot.workspace_id],
            )
            .map_err(sqlite_store_error)?;
            conn.execute(
                r#"
                INSERT INTO crew_performance_snapshot (
                    workspace_id, computed_at, window_days, types_hash, types_json
                ) VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![
                    snapshot.workspace_id,
                    stamp(snapshot.computed_at),
                    snapshot.window_days,
                    snapshot.types_hash,
                    snapshot.types_json,
                ],
            )
            .map_err(sqlite_store_error)?;
            for cell in &snapshot.cells {
                conn.execute(
                    r#"
                    INSERT INTO crew_performance_cells (
                        workspace_id, crew, model, tier, task_type,
                        n, raw, crew_mean, score
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    "#,
                    params![
                        snapshot.workspace_id,
                        cell.crew,
                        cell.model,
                        cell.tier,
                        cell.task_type,
                        cell.n,
                        cell.raw,
                        cell.crew_mean,
                        cell.score,
                    ],
                )
                .map_err(sqlite_store_error)?;
            }
            Ok(())
        })
    }

    fn crew_performance_snapshot(
        &self,
        workspace_id: &str,
    ) -> Result<Option<CrewPerformanceSnapshot>, OrbitError> {
        let conn = self.read()?;
        let meta = conn
            .query_row(
                r#"
                SELECT computed_at, window_days, types_hash, types_json
                FROM crew_performance_snapshot
                WHERE workspace_id = ?1
                "#,
                params![workspace_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_store_error)?;
        let Some((computed_at, window_days, types_hash, types_json)) = meta else {
            return Ok(None);
        };
        let mut statement = conn
            .prepare(
                r#"
                SELECT crew, model, tier, task_type, n, raw, crew_mean, score
                FROM crew_performance_cells
                WHERE workspace_id = ?1
                ORDER BY crew, model, tier, task_type
                "#,
            )
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id], |row| {
                Ok(CrewPerformanceCell {
                    crew: row.get(0)?,
                    model: row.get(1)?,
                    tier: row.get(2)?,
                    task_type: row.get(3)?,
                    n: row.get(4)?,
                    raw: row.get(5)?,
                    crew_mean: row.get(6)?,
                    score: row.get(7)?,
                })
            })
            .map_err(sqlite_store_error)?;
        let cells = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        Ok(Some(CrewPerformanceSnapshot {
            workspace_id: workspace_id.to_string(),
            computed_at: crate::parse_timestamp(&computed_at).map_err(sqlite_store_error)?,
            window_days,
            types_hash,
            types_json,
            cells,
        }))
    }
}
