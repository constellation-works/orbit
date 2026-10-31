use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::assistance::{AssistanceDisposition, AssistanceResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::contract::{
    DeliveryQualityApplicability, DeliveryQualityIdentity, DeliveryQualityProvenance,
    FrozenDeliveryQuality,
};

/// Host-selected, bounded page. Implementations read durable evidence only;
/// they must not use current task crew or later outcomes to repair history.
pub struct DeliveryQualityPage {
    pub units: Vec<FrozenDeliveryQuality>,
    pub next_cursor: String,
}
pub trait DeliveryQualitySource {
    fn delivery_quality_page(&self, after: &str) -> Result<DeliveryQualityPage, OrbitError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityDimension {
    pub ordinal: Option<u8>,
    pub choice: Option<String>,
    pub disposition: Option<AssistanceDisposition>,
    pub applicability: DeliveryQualityApplicability,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryQualityProfile {
    pub schema_version: u32,
    pub assessment_id: String,
    pub unit_id: String,
    pub identity: DeliveryQualityIdentity,
    pub provenance: DeliveryQualityProvenance,
    pub frozen_at: DateTime<Utc>,
    pub evidence_digest: String,
    pub work_id: Option<String>,
    pub evidence_refs: Vec<String>,
    pub policy_epoch: String,
    pub question_version: String,
    pub rubric_version: String,
    pub builder_version: String,
    pub grader: Option<String>,
    pub grader_model: Option<String>,
    pub grader_revision: Option<String>,
    pub gaps: Vec<String>,
    pub non_code: bool,
    pub result: Option<AssistanceResult>,
    pub dimensions: BTreeMap<String, DeliveryQualityDimension>,
}
