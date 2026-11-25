//! Concrete repositories for derived per-task analytics fact caches
//! (`codebase_task_facts` and `agent_task_facts`).
//!
//! Reached only through [`Snapshot::codebase_fact`], [`Tx::codebase_fact`],
//! [`Snapshot::agent_fact`], and [`Tx::agent_fact`].
//!
//! Both tables are created in the ordered schema list (v91 `agent_task_facts`
//! and v96 `codebase_task_facts`). Because dropping either derived-cache table
//! while the store is open is a documented reset path, reads return empty and
//! writes rebuild the table only when the table is missing; normal writes run
//! no DDL.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, params};

use crate::contracts::{AgentTaskFactsRow, CodebaseTaskFactsRow};
use crate::store::{Snapshot, Tx};

const CODEBASE_TABLE: &str = "codebase_task_facts";
const AGENT_TABLE: &str = "agent_task_facts";

/// Concrete codebase-facts repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct CodebaseFactRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> CodebaseFactRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> CodebaseFactRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Upsert `rows` for `workspace_id`.
    pub fn put_many(
        &self,
        workspace_id: &str,
        rows: &[CodebaseTaskFactsRow],
    ) -> Result<(), OrbitError> {
        if rows.is_empty() {
            return Ok(());
        }
        ensure_cache_table_if_dropped(self.conn, CODEBASE_TABLE)?;
        let mut upsert = self
            .conn
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
        Ok(())
    }

    /// Delete stored facts for `task_ids` in `workspace_id`.
    pub fn delete_many(&self, workspace_id: &str, task_ids: &[String]) -> Result<(), OrbitError> {
        if task_ids.is_empty() || !table_exists(self.conn, CODEBASE_TABLE)? {
            return Ok(());
        }
        let mut delete = self
            .conn
            .prepare("DELETE FROM codebase_task_facts WHERE workspace_id = ?1 AND task_id = ?2")
            .map_err(sqlite_store_error)?;
        for task_id in task_ids {
            delete
                .execute(params![workspace_id, task_id])
                .map_err(sqlite_store_error)?;
        }
        Ok(())
    }
}

impl<Access> CodebaseFactRepository<'_, Access> {
    /// Every stored codebase task-fact row for `workspace_id`.
    pub fn list(&self, workspace_id: &str) -> Result<Vec<CodebaseTaskFactsRow>, OrbitError> {
        if !table_exists(self.conn, CODEBASE_TABLE)? {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
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
}

/// Concrete agent-facts repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct AgentFactRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> AgentFactRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> AgentFactRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Upsert `rows` for `workspace_id`.
    pub fn put_many(
        &self,
        workspace_id: &str,
        rows: &[AgentTaskFactsRow],
    ) -> Result<(), OrbitError> {
        if rows.is_empty() {
            return Ok(());
        }
        ensure_cache_table_if_dropped(self.conn, AGENT_TABLE)?;
        let mut upsert = self
            .conn
            .prepare(
                "INSERT INTO agent_task_facts
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
        Ok(())
    }

    /// Delete stored facts for `task_ids` in `workspace_id`.
    pub fn delete_many(&self, workspace_id: &str, task_ids: &[String]) -> Result<(), OrbitError> {
        if task_ids.is_empty() || !table_exists(self.conn, AGENT_TABLE)? {
            return Ok(());
        }
        let mut delete = self
            .conn
            .prepare("DELETE FROM agent_task_facts WHERE workspace_id = ?1 AND task_id = ?2")
            .map_err(sqlite_store_error)?;
        for task_id in task_ids {
            delete
                .execute(params![workspace_id, task_id])
                .map_err(sqlite_store_error)?;
        }
        Ok(())
    }
}

impl<Access> AgentFactRepository<'_, Access> {
    /// Every stored agent task-fact row for `workspace_id`.
    pub fn list(&self, workspace_id: &str) -> Result<Vec<AgentTaskFactsRow>, OrbitError> {
        if !table_exists(self.conn, AGENT_TABLE)? {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, format_version, source, facts_json FROM agent_task_facts
                 WHERE workspace_id = ?1",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| {
            Ok(AgentTaskFactsRow {
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
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .map_err(sqlite_store_error)
}

/// Rebuild a derived-cache table only if an external reset dropped it after
/// `Store::open` ran schema migrations v91 (`agent_task_facts`) and v96
/// (`codebase_task_facts`). Normal writes never enter this branch.
fn ensure_cache_table_if_dropped(conn: &Connection, table: &str) -> Result<(), OrbitError> {
    if table_exists(conn, table)? {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (
            workspace_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            format_version INTEGER NOT NULL,
            source TEXT NOT NULL,
            facts_json TEXT NOT NULL,
            PRIMARY KEY (workspace_id, task_id)
        ) WITHOUT ROWID;",
    ))
    .map_err(sqlite_store_error)
}
