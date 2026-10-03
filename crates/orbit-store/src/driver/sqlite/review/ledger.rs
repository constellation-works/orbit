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

/// Write the ledger, fencing on the revision the caller read.
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
                "INSERT INTO review_lineages VALUES (?1,?2,?3,?4)",
                params![
                    workspace_id,
                    ledger.lineage_key,
                    ledger.revision,
                    encode(ledger)?
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        }
        Some(previous) => {
            ledger.revision = previous.saturating_add(1);
            let changed = conn
                .execute(
                    "UPDATE review_lineages SET revision=?1, ledger_json=?2 WHERE workspace_id=?3 AND lineage_key=?4 AND revision=?5",
                    params![
                        ledger.revision,
                        encode(ledger)?,
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
