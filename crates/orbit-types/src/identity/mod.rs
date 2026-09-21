//! Domain contracts for this Orbit types module.

mod actor;
mod agent_family;
mod agent_pair;
mod artifact_ids;
mod error;
mod id;
mod machine;
pub use error::IdentityError;

#[cfg(test)]
mod tests;

pub use actor::{
    ActorIdentity, agent_from_model, normalize_attribution_label,
    normalize_optional_attribution_label, provider_for_agent_family, provider_from_model,
};
pub use agent_family::AgentFamily;
pub use agent_pair::{
    AgentModelPair, Crew, CrewAssignment, ReasoningEffort, agent_family_from_cli,
    all_agent_families, infer_agent_family_from_model, normalize_agent_family_for_model,
    require_canonical_agent_family, resolve_crew, validate_antigravity_model,
};
pub use artifact_ids::{is_valid_adr_id, is_valid_friction_id, validate_friction_id};
pub use id::OrbitId;
pub use machine::{
    LEGACY_TASK_PREFIX, MACHINE_ID_PREFIX, REGISTRY_IDENTIFIER_MAX_BYTES, validate_machine_id,
    validate_machine_name, validate_new_task_prefix, validate_registry_identifier,
    validate_stored_task_prefix,
};
