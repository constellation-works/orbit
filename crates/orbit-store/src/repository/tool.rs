//! Concrete repository for registered tool records (`tools`).
//!
//! Reached only through [`Snapshot::tool`] (read-only) and [`Tx::tool`]
//! (read-write inside [`crate::Store::write`]). Whether a tool may be
//! registered, removed or toggled is the calling operation's rule; this
//! repository owns the SQL and the row mapping only.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::tool::StoredTool;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::now_string;
use crate::store::{Snapshot, Tx};

const TOOL_COLUMNS: &str = "name, path, description, parameters_json, enabled, builtin";

/// Concrete tool repository scoped to a [`Snapshot`] (read-only) or a [`Tx`]
/// (read-write).
pub struct ToolRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ToolRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ToolRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert a new tool row.
    pub fn put(&self, tool: &StoredTool) -> Result<(), OrbitError> {
        let parameters_json = serialize_tool_parameters(tool)?;
        self.conn
            .execute(
                "INSERT INTO tools(name, path, description, parameters_json, enabled, builtin, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    tool.name,
                    tool.path,
                    tool.description,
                    parameters_json,
                    tool.enabled as i32,
                    tool.builtin as i32,
                    now_string(),
                    now_string(),
                ],
            )
            .map_err(sqlite_store_error)?;

        Ok(())
    }

    /// Delete the tool named `name`. Returns whether a row was deleted.
    pub fn delete(&self, name: &str) -> Result<bool, OrbitError> {
        let affected = self
            .conn
            .execute("DELETE FROM tools WHERE name = ?1", params![name])
            .map_err(sqlite_store_error)?;

        Ok(affected > 0)
    }

    /// Set the `enabled` flag of the tool named `name`. Returns whether a row
    /// was updated.
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<bool, OrbitError> {
        let affected = self
            .conn
            .execute(
                "UPDATE tools SET enabled = ?1, updated_at = ?2 WHERE name = ?3",
                params![enabled as i32, now_string(), name],
            )
            .map_err(sqlite_store_error)?;

        Ok(affected > 0)
    }
}

impl<Access> ToolRepository<'_, Access> {
    /// Every registered tool, ordered by name.
    pub fn list(&self) -> Result<Vec<StoredTool>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {TOOL_COLUMNS} FROM tools ORDER BY name"))
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map([], tool_from_row)
            .map_err(sqlite_store_error)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// The tool named `name`, if registered.
    pub fn get(&self, name: &str) -> Result<Option<StoredTool>, OrbitError> {
        self.conn
            .query_row(
                &format!("SELECT {TOOL_COLUMNS} FROM tools WHERE name = ?1"),
                params![name],
                tool_from_row,
            )
            .optional()
            .map_err(sqlite_store_error)
    }
}

fn tool_from_row(row: &Row<'_>) -> rusqlite::Result<StoredTool> {
    let parameters_json: String = row.get(3)?;
    Ok(StoredTool {
        name: row.get(0)?,
        path: row.get(1)?,
        description: row.get(2)?,
        parameters: parse_tool_parameters(&parameters_json)?,
        enabled: row.get::<_, i32>(4)? != 0,
        builtin: row.get::<_, i32>(5)? != 0,
    })
}

fn parse_tool_parameters(raw: &str) -> rusqlite::Result<Vec<orbit_types::tool::ToolParam>> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn serialize_tool_parameters(tool: &StoredTool) -> Result<String, OrbitError> {
    serde_json::to_string(&tool.parameters)
        .map_err(|error| OrbitError::Store(format!("serialize tool parameters: {error}")))
}
