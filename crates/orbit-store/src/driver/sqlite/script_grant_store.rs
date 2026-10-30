//! Host-local script grant records (`script_grant`, beside `plugins`).
//!
//! A row here is only half of a grant: the witness under the global
//! root's `scripts/.grants/` is the other half, and the runtime refuses a
//! row whose witness does not match. This module only persists rows.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::activity_job::{
    ScriptGrant, ScriptGrantLimits, ScriptHostRole, ScriptNetwork,
};
use rusqlite::{OptionalExtension, Row, params};

use crate::Store;

const SCRIPT_GRANT_COLUMNS: &str = "workspace_id, name, grant_id, digest, manifest_digest, \
     entry_digest, interpreter_paths_json, network, read_roots_json, write_roots_json, \
     env_pass_json, limits_json, host_role, grantor, granted_at, provenance, expires_at";

fn json_column<T: serde::de::DeserializeOwned>(row: &Row<'_>, index: usize) -> rusqlite::Result<T> {
    let raw: String = row.get(index)?;
    serde_json::from_str(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn script_grant_from_row(row: &Row<'_>) -> rusqlite::Result<ScriptGrant> {
    let network_raw: String = row.get(7)?;
    let host_role_raw: String = row.get(12)?;
    // An unparseable stored limits blob reads as the default ceilings
    // rather than as no limits; the witness check above this layer is
    // what refuses a tampered row, and it digests the typed record.
    let limits_raw: String = row.get(11)?;
    let limits: ScriptGrantLimits = serde_json::from_str(&limits_raw).unwrap_or_default();
    Ok(ScriptGrant {
        workspace_id: row.get(0)?,
        name: row.get(1)?,
        grant_id: row.get(2)?,
        digest: row.get(3)?,
        manifest_digest: row.get(4)?,
        entry_digest: row.get(5)?,
        interpreter_paths: json_column(row, 6)?,
        network: ScriptNetwork::parse(&network_raw).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                format!("unknown script network `{network_raw}`").into(),
            )
        })?,
        read_roots: json_column(row, 8)?,
        write_roots: json_column(row, 9)?,
        env_pass: json_column(row, 10)?,
        limits,
        host_role: ScriptHostRole::parse(&host_role_raw).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                format!("unknown script host role `{host_role_raw}`").into(),
            )
        })?,
        grantor: row.get(13)?,
        granted_at: row.get(14)?,
        provenance: row.get(15)?,
        expires_at: row.get(16)?,
    })
}

fn serialize_json<T: serde::Serialize>(value: &T, what: &str) -> Result<String, OrbitError> {
    serde_json::to_string(value)
        .map_err(|error| OrbitError::Store(format!("serialize script grant {what}: {error}")))
}

impl Store {
    pub fn list_script_grants(&self, workspace_id: &str) -> Result<Vec<ScriptGrant>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SCRIPT_GRANT_COLUMNS} FROM script_grant \
                 WHERE workspace_id = ?1 ORDER BY name"
            ))
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(params![workspace_id], script_grant_from_row)
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    pub fn get_script_grant(
        &self,
        workspace_id: &str,
        name: &str,
    ) -> Result<Option<ScriptGrant>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {SCRIPT_GRANT_COLUMNS} FROM script_grant \
                 WHERE workspace_id = ?1 AND name = ?2"
            ))
            .map_err(sqlite_store_error)?;
        stmt.query_row(params![workspace_id, name], script_grant_from_row)
            .optional()
            .map_err(sqlite_store_error)
    }

    /// Insert or replace the grant for `(workspace_id, name)`.
    pub fn upsert_script_grant(&self, grant: &ScriptGrant) -> Result<(), OrbitError> {
        let interpreter_paths_json = serialize_json(&grant.interpreter_paths, "interpreter paths")?;
        let read_roots_json = serialize_json(&grant.read_roots, "read roots")?;
        let write_roots_json = serialize_json(&grant.write_roots, "write roots")?;
        let env_pass_json = serialize_json(&grant.env_pass, "env pass")?;
        let limits_json = serialize_json(&grant.limits, "limits")?;
        self.with_transaction(|tx| {
            tx.tx
                .execute(
                    "INSERT INTO script_grant(workspace_id, name, grant_id, digest, \
                     manifest_digest, entry_digest, interpreter_paths_json, network, \
                     read_roots_json, write_roots_json, env_pass_json, limits_json, \
                     host_role, grantor, granted_at, provenance, expires_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
                     ON CONFLICT(workspace_id, name) DO UPDATE SET grant_id = excluded.grant_id, \
                     digest = excluded.digest, manifest_digest = excluded.manifest_digest, \
                     entry_digest = excluded.entry_digest, \
                     interpreter_paths_json = excluded.interpreter_paths_json, \
                     network = excluded.network, read_roots_json = excluded.read_roots_json, \
                     write_roots_json = excluded.write_roots_json, \
                     env_pass_json = excluded.env_pass_json, limits_json = excluded.limits_json, \
                     host_role = excluded.host_role, grantor = excluded.grantor, \
                     granted_at = excluded.granted_at, provenance = excluded.provenance, \
                     expires_at = excluded.expires_at",
                    params![
                        grant.workspace_id,
                        grant.name,
                        grant.grant_id,
                        grant.digest,
                        grant.manifest_digest,
                        grant.entry_digest,
                        interpreter_paths_json,
                        grant.network.as_str(),
                        read_roots_json,
                        write_roots_json,
                        env_pass_json,
                        limits_json,
                        grant.host_role.as_str(),
                        grant.grantor,
                        grant.granted_at,
                        grant.provenance,
                        grant.expires_at,
                    ],
                )
                .map_err(sqlite_store_error)?;
            Ok(())
        })
    }

    pub fn delete_script_grant(&self, workspace_id: &str, name: &str) -> Result<bool, OrbitError> {
        self.with_transaction(|tx| {
            let affected = tx
                .tx
                .execute(
                    "DELETE FROM script_grant WHERE workspace_id = ?1 AND name = ?2",
                    params![workspace_id, name],
                )
                .map_err(sqlite_store_error)?;
            Ok(affected > 0)
        })
    }
}
