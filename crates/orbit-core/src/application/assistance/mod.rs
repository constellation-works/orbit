//! Optional assistance application port. Consumers supply immutable, selected
//! evidence; this service cannot mutate tasks, reviews, human answers or jobs.

mod preparation;
mod templates;

mod service;
pub use service::AssistanceService;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_store::contracts::assistance::AssistanceClaim;
use orbit_types::assistance::{AssistanceReason, AssistanceRequest, AssistanceResult};

pub(crate) use preparation::prepare_assistance_evidence;

/// The assistance service for the private task-quality, delivery-quality and
/// shadow precedent-decision consumers. Composition implements it on
/// `OrbitRuntime`, joining host-only settings, the concrete adapter and the
/// assistance ledger, so these use cases never name composition [ORB-22005].
pub(crate) trait AssistanceServicePort {
    /// A service over this host's assistance ledger, bound to its global config.
    fn assistance_service(&self) -> Result<AssistanceService, OrbitError>;
}

/// Inject a recorded adapter at the same public boundary used by consumers.
/// Implementations must honor the claim's timeout and bounded response size,
/// never log request/response/credential values, and return fixed error codes.
pub trait AssistanceAdapter: Send + Sync {
    /// Stable identity recorded on every admitted work item. The
    /// provider-neutral service must not name a concrete adapter; each
    /// implementation reports its own.
    fn assistance_adapter_identity(&self) -> &str;
    /// Resolve credentials only within the adapter and redact their values
    /// along with supplied secrets before any persistence or transmission.
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
        policy: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason>;
    /// A refusal decided before outbound I/O must be one of the reasons that
    /// `refusal_precedes_outbound_io` lists; any other result is an attempted request.
    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        policy: &AssistancePolicy,
    ) -> AssistanceResult;
}

/// Reloaded before admission, before sending, after sending and before reads.
/// The composition loader reads only the operator's global configuration.
pub type AssistanceSettingsLoader =
    Arc<dyn Fn() -> Result<AssistanceSettings, OrbitError> + Send + Sync>;
pub type AssistanceClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// What identifies one live host grant: its digest (which covers the policy,
/// including its model revision, and the shared limits and budget), and the
/// policy version and model revision it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistancePolicyIdentity {
    pub digest: String,
    pub policy_version: String,
    pub model_revision: String,
}
