//! Provider-neutral, non-authoritative decision assistance, compatible with
//! the `decision-assistance-eval/1` packet and answer vocabulary.

use super::AssistanceReason;
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const ASSISTANCE_FORMAT: &str = "decision-assistance-eval/1";
pub const ASSISTANCE_RESULT_SCHEMA: &str = "decision-assistance-result/1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistanceMode {
    #[default]
    Off,
    Shadow,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceCriterionRef {
    pub index: u32,
    pub text_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssistanceQuestion {
    Predicate {
        rubric: String,
        /// Evaluation-owned confidence threshold; absent means uncertain.
        min_confidence: Option<f64>,
        #[serde(default)]
        criterion_refs: Vec<AssistanceCriterionRef>,
    },
    Choice {
        rubric: String,
        allowed: Vec<String>,
        #[serde(default)]
        abstain_values: Vec<String>,
        min_confidence: Option<f64>,
        #[serde(default)]
        criterion_refs: Vec<AssistanceCriterionRef>,
    },
}

impl AssistanceQuestion {
    pub fn rubric(&self) -> &str {
        match self {
            Self::Predicate { rubric, .. } | Self::Choice { rubric, .. } => rubric,
        }
    }

    pub fn criterion_refs(&self) -> &[AssistanceCriterionRef] {
        match self {
            Self::Predicate { criterion_refs, .. } | Self::Choice { criterion_refs, .. } => {
                criterion_refs
            }
        }
    }

    pub fn min_confidence(&self) -> Option<f64> {
        match self {
            Self::Predicate { min_confidence, .. } | Self::Choice { min_confidence, .. } => {
                *min_confidence
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceContract {
    pub format_version: String,
    pub question_set_version: String,
    pub rubric_version: String,
    pub evidence_builder_version: String,
    pub result_schema_version: String,
    pub questions: BTreeMap<String, AssistanceQuestion>,
}

impl AssistanceContract {
    pub fn validate_contract(&self) -> Result<(), AssistanceReason> {
        if self.format_version != ASSISTANCE_FORMAT
            || self.result_schema_version != ASSISTANCE_RESULT_SCHEMA
            || self.questions.is_empty()
            || self.questions.len() > 400
            || [
                &self.question_set_version,
                &self.rubric_version,
                &self.evidence_builder_version,
            ]
            .iter()
            .any(|s| !assistance_identifier(s))
        {
            return Err(AssistanceReason::InvalidRequest);
        }
        for (name, q) in &self.questions {
            if !assistance_identifier(name)
                || q.rubric().is_empty()
                || q.rubric().len() > 4096
                || q.min_confidence()
                    .is_some_and(|p| !assistance_probability(p))
                || q.criterion_refs().len() > 128
                || q.criterion_refs()
                    .iter()
                    .any(|r| !assistance_is_digest(&r.text_digest))
            {
                return Err(AssistanceReason::InvalidRequest);
            }
            if let AssistanceQuestion::Choice {
                allowed,
                abstain_values,
                ..
            } = q
                && (!(2..=255).contains(&allowed.len())
                    || allowed.iter().any(|s| !assistance_identifier(s))
                    || allowed.iter().collect::<BTreeSet<_>>().len() != allowed.len()
                    || abstain_values.iter().any(|s| !allowed.contains(s)))
            {
                return Err(AssistanceReason::InvalidRequest);
            }
        }
        Ok(())
    }
}

/// The evaluator's v1 envelope, extended with consumer-owned bounded fields.
/// No referenced content is fetched by the assistance service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistanceEvidencePacket {
    pub packet_id: String,
    pub subject_id: String,
    pub collected_at: DateTime<Utc>,
    pub evidence_refs: Vec<String>,
    pub omissions: Vec<String>,
    pub truncated: bool,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceRequest {
    pub workspace_id: String,
    pub owner_id: String,
    pub use_case: String,
    /// Digest of the consumer's semantic input, before outbound minimization.
    pub source_digest: String,
    pub contract: AssistanceContract,
    pub evidence_packet: AssistanceEvidencePacket,
}

impl AssistanceRequest {
    pub fn validate_request(&self) -> Result<(), AssistanceReason> {
        self.contract.validate_contract()?;
        if [
            &self.workspace_id,
            &self.owner_id,
            &self.use_case,
            &self.evidence_packet.packet_id,
            &self.evidence_packet.subject_id,
        ]
        .iter()
        .any(|s| !assistance_identifier(s))
            || !assistance_is_digest(&self.source_digest)
            || self.evidence_packet.evidence_refs.len() > 128
            || self.evidence_packet.omissions.len() > 128
            || self
                .evidence_packet
                .evidence_refs
                .iter()
                .chain(&self.evidence_packet.omissions)
                .any(|s| s.len() > 512)
            || self.evidence_packet.fields.len() > 128
            || self.evidence_packet.fields.keys().any(|k| k.len() > 512)
            || self.evidence_packet.fields.keys().any(|k| {
                matches!(
                    k.as_str(),
                    "packet_id"
                        | "subject_id"
                        | "collected_at"
                        | "evidence_refs"
                        | "omissions"
                        | "truncated"
                )
            })
        {
            return Err(AssistanceReason::InvalidRequest);
        }
        let mut pending: Vec<_> = self
            .evidence_packet
            .fields
            .values()
            .map(|v| (v, 0_u8))
            .collect();
        let mut count = 0_usize;
        let mut text_bytes = self
            .evidence_packet
            .fields
            .keys()
            .map(String::len)
            .sum::<usize>();
        while let Some((value, depth)) = pending.pop() {
            count += 1;
            if count > 10_000 || depth > 16 || text_bytes > 65_536 {
                return Err(AssistanceReason::InvalidRequest);
            }
            match value {
                Value::String(s) => text_bytes = text_bytes.saturating_add(s.len()),
                Value::Array(a) => {
                    if a.len() + pending.len() > 10_000 {
                        return Err(AssistanceReason::InvalidRequest);
                    }
                    pending.extend(a.iter().map(|v| (v, depth + 1)));
                }
                Value::Object(o) => {
                    if o.len() + pending.len() > 10_000 || o.keys().any(|k| k.len() > 512) {
                        return Err(AssistanceReason::InvalidRequest);
                    }
                    text_bytes =
                        text_bytes.saturating_add(o.keys().map(String::len).sum::<usize>());
                    pending.extend(o.values().map(|v| (v, depth + 1)));
                }
                _ => {}
            }
        }
        if text_bytes > 65_536 {
            return Err(AssistanceReason::InvalidRequest);
        }
        for reference in self
            .contract
            .questions
            .values()
            .flat_map(AssistanceQuestion::criterion_refs)
        {
            let criterion = self
                .evidence_packet
                .fields
                .get("criteria")
                .and_then(Value::as_array)
                .and_then(|criteria| {
                    criteria.iter().find(|c| {
                        c.get("index").and_then(Value::as_u64) == Some(reference.index.into())
                    })
                });
            let Some(criterion) = criterion else {
                return Err(AssistanceReason::InvalidRequest);
            };
            if criterion.get("text_digest").and_then(Value::as_str) != Some(&reference.text_digest)
                || criterion
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| assistance_digest(text.as_bytes()) != reference.text_digest)
            {
                return Err(AssistanceReason::InvalidRequest);
            }
        }
        Ok(())
    }
}

pub fn assistance_probability(p: f64) -> bool {
    p.is_finite() && (0.0..=1.0).contains(&p)
}

pub fn assistance_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-./:".contains(&c))
}

pub fn assistance_is_digest(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}

pub fn assistance_digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
