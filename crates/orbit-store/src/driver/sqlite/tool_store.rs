use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::tool::StoredTool;
use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreTx, now_string};

impl Store {
    pub fn list_tools(&self) -> Result<Vec<StoredTool>, OrbitError> {
        let conn = self.read()?;

        let mut stmt = conn
            .prepare(
                "SELECT name, path, description, parameters_json, enabled, builtin FROM tools ORDER BY name",
            )
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map([], |row| {
                let parameters_json: String = row.get(3)?;
                Ok(StoredTool {
                    name: row.get(0)?,
                    path: row.get(1)?,
                    description: row.get(2)?,
                    parameters: parse_tool_parameters(&parameters_json)?,
                    enabled: row.get::<_, i32>(4)? != 0,
                    builtin: row.get::<_, i32>(5)? != 0,
                })
            })
            .map_err(sqlite_store_error)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    pub fn get_tool(&self, name: &str) -> Result<Option<StoredTool>, OrbitError> {
        let conn = self.read()?;

        let mut stmt = conn
            .prepare(
                "SELECT name, path, description, parameters_json, enabled, builtin FROM tools WHERE name = ?1",
            )
            .map_err(sqlite_store_error)?;

        let result = stmt
            .query_row(params![name], |row| {
                let parameters_json: String = row.get(3)?;
                Ok(StoredTool {
                    name: row.get(0)?,
                    path: row.get(1)?,
                    description: row.get(2)?,
                    parameters: parse_tool_parameters(&parameters_json)?,
                    enabled: row.get::<_, i32>(4)? != 0,
                    builtin: row.get::<_, i32>(5)? != 0,
                })
            })
            .optional()
            .map_err(sqlite_store_error)?;

        Ok(result)
    }
}

impl<'a> StoreTx<'a> {
    pub fn insert_tool(&mut self, tool: &StoredTool) -> Result<(), OrbitError> {
        let parameters_json = serialize_tool_parameters(tool)?;
        self.tx
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

    pub fn delete_tool(&mut self, name: &str) -> Result<bool, OrbitError> {
        let affected = self
            .tx
            .execute("DELETE FROM tools WHERE name = ?1", params![name])
            .map_err(sqlite_store_error)?;

        Ok(affected > 0)
    }

    pub fn set_tool_enabled(&mut self, name: &str, enabled: bool) -> Result<bool, OrbitError> {
        let affected = self
            .tx
            .execute(
                "UPDATE tools SET enabled = ?1, updated_at = ?2 WHERE name = ?3",
                params![enabled as i32, now_string(), name],
            )
            .map_err(sqlite_store_error)?;

        Ok(affected > 0)
    }
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
