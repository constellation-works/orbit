//! Review ledger JSON codec and revision-fenced ledger reads and writes.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::ReviewLedger;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|error| OrbitError::Store(error.to_string()))
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|error| OrbitError::Store(format!("invalid persisted review record: {error}")))
}

pub(super) fn read_ledger(
    conn: &Connection,
    workspace_id: &str,
    lineage_key: &str,
) -> Result<Option<ReviewLedger>, OrbitError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT ledger_json FROM review_lineages WHERE workspace_id=?1 AND lineage_key=?2",
            params![workspace_id, lineage_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    raw.as_deref().map(decode).transpose()
}

/// Every ledger with an attempt `run_id` still holds.
pub(super) fn ledgers_held_by(
    conn: &Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Vec<ReviewLedger>, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT ledger_json FROM review_lineages WHERE workspace_id=?1 AND holder_run_id=?2",
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = statement
        .query_map(params![workspace_id, run_id], |row| row.get::<_, String>(0))
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    rows.map(|raw| {
        raw.map_err(|error| OrbitError::Store(error.to_string()))
            .and_then(|raw| decode(&raw))
    })
    .collect()
}

/// Write the ledger, fencing on the revision the caller read. The holder
/// column mirrors the run still holding an attempt, so a terminating run
/// finds what it must release without decoding every ledger.
pub(super) fn write_ledger(
    conn: &Connection,
    workspace_id: &str,
    previous_revision: Option<u32>,
    ledger: &mut ReviewLedger,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    ledger.updated_at = now;
    match previous_revision {
        None => {
            ledger.revision = 1;
            conn.execute(
                "INSERT INTO review_lineages (workspace_id, lineage_key, revision, ledger_json, holder_run_id) VALUES (?1,?2,?3,?4,?5)",
                params![
                    workspace_id,
                    ledger.lineage_key,
                    ledger.revision,
                    encode(ledger)?,
                    ledger.holder_run_id(),
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        }
        Some(previous) => {
            ledger.revision = previous.saturating_add(1);
            let changed = conn
                .execute(
                    "UPDATE review_lineages SET revision=?1, ledger_json=?2, holder_run_id=?3 WHERE workspace_id=?4 AND lineage_key=?5 AND revision=?6",
                    params![
                        ledger.revision,
                        encode(ledger)?,
                        ledger.holder_run_id(),
                        workspace_id,
                        ledger.lineage_key,
                        previous
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if changed == 0 {
                return Err(OrbitError::Store(
                    "review ledger changed concurrently; reread and retry".into(),
                ));
            }
        }
    }
    Ok(())
}
