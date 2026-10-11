use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::Store;

use crate::contracts::V2AuditEventInsertParams;

impl Store {
    pub fn insert_v2_audit_event(
        &self,
        params: &V2AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            r#"INSERT OR IGNORE INTO v2_audit_events(
                workspace_id, event_id, source, schema_version, event_type, ts,
                run_id, agent_identity, parent_event_id, workspace_path, payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            rusqlite::params![
                params.workspace_id,
                params.event_id,
                params.source,
                i64::from(params.schema_version),
                params.event_type,
                params.ts.to_rfc3339(),
                params.run_id,
                params.agent_identity,
                params.parent_event_id,
                params.workspace_path,
                params.payload_json,
            ],
        )
        .map_err(sqlite_store_error)?;
        Ok(())
    }
}
