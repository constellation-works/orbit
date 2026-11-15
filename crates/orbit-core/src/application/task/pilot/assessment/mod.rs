//! Validation of an agent's task-pilot assessment: selectors and recommendations.

mod host_requirements;
mod recommendations;
mod review_evidence;
mod selectors;

pub(super) use host_requirements::{required_machine, required_os, routing_os, user_facing_ui};
pub(in super::super) use recommendations::member_ready;
pub(super) use recommendations::{
    contract_conflicts, normalize_evidence_gaps, validate_recommendations,
};
pub(super) use review_evidence::{retained_review_evidence, review_filed, reviewed_evidence_drops};
pub(super) use selectors::{SelectorCheck, unauthorized_missing_targets, validate_after_selectors};
