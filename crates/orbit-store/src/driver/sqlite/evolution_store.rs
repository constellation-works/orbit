//! Atomic signature corroboration and checkpoint replay deduplication.
use crate::{
    Store,
    contracts::{EvolutionDecision, EvolutionStoreBackend},
};
use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use orbit_types::record::{EvolutionOccurrence, EvolutionRecord, EvolutionState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

fn decode_record(json: String) -> Result<EvolutionRecord, OrbitError> {
    serde_json::from_str(&json)
        .map_err(|error| OrbitError::Store(format!("invalid evolution record: {error}")))
}
fn save_record(conn: &Connection, record: &EvolutionRecord) -> Result<(), OrbitError> {
    let json =
        serde_json::to_string(record).map_err(|error| OrbitError::Store(error.to_string()))?;
    let state =
        serde_json::to_value(record.state).map_err(|error| OrbitError::Store(error.to_string()))?;
    conn.execute("INSERT INTO evolution_records(id, workspace_id, signature, state, record_json) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(id) DO UPDATE SET state=excluded.state, record_json=excluded.record_json",
        params![record.id, record.workspace, record.signature, state.as_str(), json]).map_err(sqlite_store_error)?;
    Ok(())
}
impl EvolutionStoreBackend for Store {
    fn record_evolution_costs(
        &self,
        workspace: &str,
        events: &[EvolutionOccurrence],
    ) -> Result<Vec<EvolutionRecord>, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut records = Vec::new();
            for event in events {
                event.cost.validate_cost_event().map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM evolution_costs WHERE workspace_id=?1 AND source_key=?2)", params![workspace, event.source_key], |row| row.get(0)).map_err(sqlite_store_error)?;
                if exists { continue; }
                let record = if let Some(signature) = &event.cost.signature {
                    let json: Option<String> = conn.query_row("SELECT record_json FROM evolution_records WHERE workspace_id=?1 AND signature=?2 AND state <> 'resolved'", params![workspace, signature], |row| row.get(0)).optional().map_err(sqlite_store_error)?;
                    let mut record = json.map(decode_record).transpose()?.unwrap_or_else(|| EvolutionRecord {
                        id: orbit_common::observability::audit_id::audit_execution_id("EV"), workspace: workspace.into(), signature: signature.clone(), title: event.cost.what.clone(), wished: event.cost.wished.clone(), state: EvolutionState::Open,
                        occurrences: Vec::new(), proposal_task: None, declined_until: None, cost_at_decline: 0, window_days: 14,
                    });
                    record.occurrences.push(event.clone());
                    save_record(conn, &record)?;
                    Some(record)
                } else { None };
                let json = serde_json::to_string(event).map_err(|error| OrbitError::Store(error.to_string()))?;
                conn.execute("INSERT INTO evolution_costs(workspace_id, source_key, record_id, event_json) VALUES (?1, ?2, ?3, ?4)", params![workspace, event.source_key, record.as_ref().map(|row| &row.id), json]).map_err(sqlite_store_error)?;
                if let Some(record) = record { records.push(record); }
            }
            Ok(records)
        })
    }
    fn evolution_records(&self, workspace: &str) -> Result<Vec<EvolutionRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT record_json FROM evolution_records WHERE workspace_id=?1 ORDER BY id")
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .map(|row| decode_record(row.map_err(sqlite_store_error)?))
            .collect()
    }
    fn evolution_costs(&self, workspace: &str) -> Result<Vec<EvolutionOccurrence>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT event_json FROM evolution_costs WHERE workspace_id=?1 ORDER BY source_key",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .map(|row| {
                serde_json::from_str(&row.map_err(sqlite_store_error)?)
                    .map_err(|error| OrbitError::Store(error.to_string()))
            })
            .collect()
    }
    fn mutate_evolution(
        &self,
        workspace: &str,
        id: &str,
        decide: &EvolutionDecision<'_>,
    ) -> Result<EvolutionRecord, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let json = conn
                .query_row(
                    "SELECT record_json FROM evolution_records WHERE workspace_id=?1 AND id=?2",
                    params![workspace, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sqlite_store_error)?
                .ok_or_else(|| {
                    OrbitError::InvalidInput("evolution record not found in this workspace".into())
                })?;
            let mut record = decode_record(json)?;
            decide(&mut record)?;
            save_record(conn, &record)?;
            Ok(record)
        })
    }
}
