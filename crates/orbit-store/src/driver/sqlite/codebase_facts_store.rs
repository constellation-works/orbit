//! `codebase_task_facts`: the codebase analytics scan's per-task facts, a
//! derived cache keyed by workspace and task.
//!
//! The table stays outside the migration ledger on purpose. It holds nothing
//! the task bundles cannot reproduce, each row carries its own format version
//! and the source it was computed from, and the scan treats a missing table
//! as an empty one, so dropping it is a supported reset rather than damage.
//! A ledger entry would buy none of that and would move the store's schema
//! version past every older binary, whose audit append requires an exact
//! match. The writer therefore creates the table when it is missing, and a
//! reader finds no rows until then.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::Store;
use crate::contracts::{CodebaseFactsStoreBackend, CodebaseTaskFactsRow};

const TABLE: &str = "codebase_task_facts";

fn table_exists(conn: &Connection) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [TABLE],
        |row| row.get(0),
    )
    .map_err(sqlite_store_error)
}

impl CodebaseFactsStoreBackend for Store {
    fn codebase_task_facts(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<CodebaseTaskFactsRow>, OrbitError> {
        let conn = self.read()?;
        if !table_exists(&conn)? {
            return Ok(Vec::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT task_id, format_version, source, facts_json FROM codebase_task_facts
                 WHERE workspace_id = ?1",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| {
            Ok(CodebaseTaskFactsRow {
                task_id: row.get(0)?,
                format_version: row.get(1)?,
                source: row.get(2)?,
                facts_json: row.get(3)?,
            })
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
    }

    fn write_codebase_task_facts(
        &self,
        workspace_id: &str,
        rows: &[CodebaseTaskFactsRow],
        removed: &[String],
    ) -> Result<(), OrbitError> {
        if rows.is_empty() && removed.is_empty() {
            return Ok(());
        }
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS codebase_task_facts (
                    workspace_id TEXT NOT NULL,
                    task_id TEXT NOT NULL,
                    format_version INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    facts_json TEXT NOT NULL,
                    PRIMARY KEY (workspace_id, task_id)
                ) WITHOUT ROWID;",
            )
            .map_err(sqlite_store_error)?;
            let mut upsert = conn
                .prepare(
                    "INSERT INTO codebase_task_facts
                         (workspace_id, task_id, format_version, source, facts_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (workspace_id, task_id) DO UPDATE SET
                         format_version = excluded.format_version,
                         source = excluded.source,
                         facts_json = excluded.facts_json",
                )
                .map_err(sqlite_store_error)?;
            for row in rows {
                upsert
                    .execute(params![
                        workspace_id,
                        row.task_id,
                        row.format_version,
                        row.source,
                        row.facts_json
                    ])
                    .map_err(sqlite_store_error)?;
            }
            let mut delete = conn
                .prepare("DELETE FROM codebase_task_facts WHERE workspace_id = ?1 AND task_id = ?2")
                .map_err(sqlite_store_error)?;
            for task_id in removed {
                delete
                    .execute(params![workspace_id, task_id])
                    .map_err(sqlite_store_error)?;
            }
            Ok(())
        })
    }
}
