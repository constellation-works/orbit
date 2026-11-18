use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Orbit-owned output written by the provider completion projection.
pub const MUSE_MODEL_ACCESS_OUTPUT_KEY: &str = "muse_model_access";

/// A provider-owned model refusal fails the invocation. A later admission
/// may use the scoped observation to avoid repeating that failure.
pub const MUSE_MODEL_ACCESS_REFUSED_ERROR_CODE: &str = "muse_model_access_refused";

/// An automatic admission has exhausted the configured routes after model
/// access refusals.
pub const PROVIDER_MODEL_ACCESS_HOLD_CODE: &str = "provider_model_access_hold";

/// Model-specific evidence from an actual Muse invocation. A later successful
/// invocation on the same workspace, host and account can clear a refusal.
#[derive(Debug, Serialize, Deserialize)]
pub struct MuseModelAccess {
    pub model: String,
    pub crew_account: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub refused: bool,
}
