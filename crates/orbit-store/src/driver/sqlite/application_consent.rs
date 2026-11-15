use crate::Store;
use crate::contracts::{ApplicationConsentRecord, ApplicationConsentStoreBackend};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, params};

impl ApplicationConsentStoreBackend for Store {
    fn insert_application_consent(
        &self,
        record: &ApplicationConsentRecord,
        replaces: Option<&str>,
    ) -> Result<(), OrbitError> {
        let json = serde_json::to_string(&record.consent)
            .map_err(|_| OrbitError::Store("serialize application consent".into()))?;
        self.with_transaction(|tx| {
            if let Some(id) = replaces {
                let changed = tx.tx.execute("UPDATE application_consents SET revoked = 1 WHERE consent_id = ?1 AND workspace_id = ?2 AND revoked = 0", params![id, record.consent.workspace_id]).map_err(sqlite_store_error)?;
                if changed != 1 { return Err(OrbitError::CapabilityDenied("application consent is not active in this workspace".into())); }
            }
            tx.tx.execute("INSERT INTO application_consents(consent_id, workspace_id, consent_json, fingerprint, revoked) VALUES (?1, ?2, ?3, ?4, 0)", params![record.consent.consent_id, record.consent.workspace_id, json, record.fingerprint]).map_err(sqlite_store_error)?;
            Ok(())
        })
    }
    fn get_application_consent(
        &self,
        id: &str,
    ) -> Result<Option<ApplicationConsentRecord>, OrbitError> {
        let conn = self.read()?;
        let row: Option<(String, String, bool)> = conn.query_row("SELECT consent_json, fingerprint, revoked FROM application_consents WHERE consent_id = ?1", [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional().map_err(sqlite_store_error)?;
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
    fn revoke_application_consent(&self, id: &str, workspace_id: &str) -> Result<(), OrbitError> {
        self.with_transaction(|tx| {
            let changed = tx.tx.execute("UPDATE application_consents SET revoked = 1 WHERE consent_id = ?1 AND workspace_id = ?2", params![id, workspace_id]).map_err(sqlite_store_error)?;
            if changed != 1 { return Err(OrbitError::CapabilityDenied("application consent does not belong to this workspace".into())); }
            Ok(())
        })
    }
}
