//! Provider-neutral non-authoritative assistance contracts. No I/O.
mod budget;
mod contract;
mod result;

pub use budget::{AssistanceBudget, AssistanceLimits};
pub use contract::{
    ASSISTANCE_FORMAT, ASSISTANCE_PACKET_BOUND_MAX_ID_BYTES, ASSISTANCE_PACKET_BOUND_MAX_IDS,
    ASSISTANCE_RESULT_SCHEMA, AssistanceContract, AssistanceCriterionRef, AssistanceEvidencePacket,
    AssistanceMode, AssistancePacketBoundChoice, AssistanceQuestion, AssistanceRequest,
    assistance_digest, assistance_identifier, assistance_is_digest, assistance_probability,
};
pub use result::{
    AssistanceAnswer, AssistanceDisposition, AssistanceProbability, AssistanceReason,
    AssistanceResult, AssistanceUsage, AssistanceValue,
};
