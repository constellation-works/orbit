//! Host-local installed-plugin records (`plugins`, beside `tools`).

use orbit_common::OrbitError;
use orbit_types::plugin::InstalledPlugin;
use rusqlite::{OptionalExtension, Row, params};

use crate::{Store, StoreTx, now_string};

const PLUGIN_COLUMNS: &str = "name, version, source, install_path, manifest_digest, enabled, \
     grants_json, first_party, installed_at, updated_at, certified_orbit_version";

fn plugin_from_row(row: &Row<'_>) -> rusqlite::Result<InstalledPlugin> {
    let grants_json: String = row.get(6)?;
    let grants = serde_json::from_str(&grants_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(InstalledPlugin {
        name: row.get(0)?,
        version: row.get(1)?,
        source: row.get(2)?,
        install_path: row.get(3)?,
        manifest_digest: row.get(4)?,
        enabled: row.get::<_, i32>(5)? != 0,
        grants,
        first_party: row.get::<_, i32>(7)? != 0,
        installed_at: row.get(8)?,
        updated_at: row.get(9)?,
        certified_orbit_version: row.get(10)?,
    })
}

impl Store {
    pub fn list_plugins(&self) -> Result<Vec<InstalledPlugin>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {PLUGIN_COLUMNS} FROM plugins ORDER BY name"
            ))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([], plugin_from_row)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn get_plugin(&self, name: &str) -> Result<Option<InstalledPlugin>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {PLUGIN_COLUMNS} FROM plugins WHERE name = ?1"
            ))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        stmt.query_row(params![name], plugin_from_row)
            .optional()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }
}

impl StoreTx<'_> {
    /// Insert or replace the record for `plugin.name`; `installed_at` is
    /// kept from an existing row.
    pub fn upsert_plugin(&mut self, plugin: &InstalledPlugin) -> Result<(), OrbitError> {
        let grants_json = serde_json::to_string(&plugin.grants)
            .map_err(|error| OrbitError::Store(format!("serialize plugin grants: {error}")))?;
        let now = now_string();
        self.tx
            .execute(
                "INSERT INTO plugins(name, version, source, install_path, manifest_digest, enabled, \
                 grants_json, first_party, installed_at, updated_at, certified_orbit_version) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10) \
                 ON CONFLICT(name) DO UPDATE SET version = excluded.version, \
                 source = excluded.source, install_path = excluded.install_path, \
                 manifest_digest = excluded.manifest_digest, enabled = excluded.enabled, \
                 grants_json = excluded.grants_json, first_party = excluded.first_party, \
                 updated_at = excluded.updated_at, \
                 certified_orbit_version = excluded.certified_orbit_version",
                params![
                    plugin.name,
                    plugin.version,
                    plugin.source,
                    plugin.install_path,
                    plugin.manifest_digest,
                    plugin.enabled as i32,
                    grants_json,
                    plugin.first_party as i32,
                    now,
                    plugin.certified_orbit_version,
                ],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(())
    }

    pub fn delete_plugin(&mut self, name: &str) -> Result<bool, OrbitError> {
        let affected = self
            .tx
            .execute("DELETE FROM plugins WHERE name = ?1", params![name])
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(affected > 0)
    }

    /// Record the Orbit version this plugin's `spec.tests` goldens passed on
    /// (design §5). A re-install keeps whatever the caller passes in the
    /// record; only a conformance run writes it here.
    pub fn set_plugin_certification(
        &mut self,
        name: &str,
        orbit_version: Option<&str>,
    ) -> Result<bool, OrbitError> {
        let affected = self
            .tx
            .execute(
                "UPDATE plugins SET certified_orbit_version = ?1, updated_at = ?2 WHERE name = ?3",
                params![orbit_version, now_string(), name],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(affected > 0)
    }

    pub fn set_plugin_enabled(
        &mut self,
        name: &str,
        enabled: bool,
        grants: &[String],
    ) -> Result<bool, OrbitError> {
        let grants_json = serde_json::to_string(grants)
            .map_err(|error| OrbitError::Store(format!("serialize plugin grants: {error}")))?;
        let affected = self
            .tx
            .execute(
                "UPDATE plugins SET enabled = ?1, grants_json = ?2, updated_at = ?3 WHERE name = ?4",
                params![enabled as i32, grants_json, now_string(), name],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(affected > 0)
    }
}
