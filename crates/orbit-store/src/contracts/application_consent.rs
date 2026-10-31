use orbit_common::OrbitError;
use orbit_types::tool::ApplicationConsent;

/// Host-local authority record. The credential itself never reaches a store.
#[derive(Debug, Clone)]
pub struct ApplicationConsentRecord {
    pub consent: ApplicationConsent,
    pub fingerprint: String,
    pub revoked: bool,
}

pub trait ApplicationConsentStoreBackend: Send + Sync {
    fn insert_application_consent(
        &self,
        record: &ApplicationConsentRecord,
        replaces: Option<&str>,
    ) -> Result<(), OrbitError>;
    fn get_application_consent(
        &self,
        id: &str,
    ) -> Result<Option<ApplicationConsentRecord>, OrbitError>;
    fn revoke_application_consent(&self, id: &str, workspace_id: &str) -> Result<(), OrbitError>;
}
