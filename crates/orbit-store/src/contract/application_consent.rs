use orbit_types::tool::ApplicationConsent;

/// Host-local authority record. The credential itself never reaches a store.
#[derive(Debug, Clone)]
pub struct ApplicationConsentRecord {
    pub consent: ApplicationConsent,
    pub fingerprint: String,
    pub revoked: bool,
}
