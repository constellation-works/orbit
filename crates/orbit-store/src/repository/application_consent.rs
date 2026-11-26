//! Concrete repository for host-local application consents
//! (`application_consents`).
//!
//! Reached only through [`Snapshot::application_consent`] (read-only) and
//! [`Tx::application_consent`] (read-write inside [`crate::Store::write`]).
//! Authority, witness verification, workspace ownership, and rotation rules
//! belong to the calling operation; this repository owns SQL queries and row
//! mapping only.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, params};

use crate::contract::ApplicationConsentRecord;
use crate::store::{Snapshot, Tx};

/// Concrete application-consent repository scoped to a [`Snapshot`] (read-only)
/// or a [`Tx`] (read-write).
pub struct ApplicationConsentRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> ApplicationConsentRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> ApplicationConsentRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert a new active (`revoked = 0`) application consent row.
    pub fn put(&self, record: &ApplicationConsentRecord) -> Result<(), OrbitError> {
        let json = serde_json::to_string(&record.consent)
            .map_err(|_| OrbitError::Store("serialize application consent".into()))?;
        self.conn
            .execute(
                "INSERT INTO application_consents(consent_id, workspace_id, consent_json, fingerprint, revoked) \
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![
                    record.consent.consent_id,
                    record.consent.workspace_id,
                    json,
                    record.fingerprint
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Mark an application consent row in `workspace_id` as revoked. Returns
    /// whether a matching row was updated.
    pub fn mark_revoked(&self, id: &str, workspace_id: &str) -> Result<bool, OrbitError> {
        let changed = self
            .conn
            .execute(
                "UPDATE application_consents SET revoked = 1 \
                 WHERE consent_id = ?1 AND workspace_id = ?2",
                params![id, workspace_id],
            )
            .map_err(sqlite_store_error)?;
        Ok(changed == 1)
    }
}

impl<Access> ApplicationConsentRepository<'_, Access> {
    /// One application consent row by `id`.
    pub fn get(&self, id: &str) -> Result<Option<ApplicationConsentRecord>, OrbitError> {
        let row: Option<(String, String, bool)> = self
            .conn
            .query_row(
                "SELECT consent_json, fingerprint, revoked FROM application_consents \
                 WHERE consent_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        row.map(|(json, fingerprint, revoked)| {
            Ok(ApplicationConsentRecord {
                consent: serde_json::from_str(&json)
                    .map_err(|_| OrbitError::Store("invalid stored application consent".into()))?,
                fingerprint,
                revoked,
            })
        })
        .transpose()
    }
}
