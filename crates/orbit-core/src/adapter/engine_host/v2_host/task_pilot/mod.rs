//! Deterministic support actions for the task-pilot workflow [ORB-10510].
//!
//! The agent leg only proposes task metadata. These actions own discovery,
//! partitioning and canonical selector validation. Ordinarily the sole write
//! is persisting assessed complexity and replacing `context_files` on the exact
//! tasks prepared for the run, with the assessment audit and replay receipt in
//! the same task-bundle commit. A CI-failure sweep may additionally request
//! explicit admission: after the selectors and every recommendation validate,
//! this boundary promotes only a current, warning-free repair from `proposed`
//! to `backlog`. An `--approve-proposed` drain requests the same promotion
//! under its own verified authority, through the ordinary approve transition.
//!
//! A selector that does not exist at the pinned source is accepted only when
//! the task's durable creation grant names it exactly. Preparation records
//! the grant's identity, apply refuses a task whose grant changed since, and
//! the atomic write compares it again under the task lock. The pilot cannot
//! drop a granted target: apply keeps any it omitted.

mod apply;
mod assessment;
mod attachment_budget;
mod drain_promotion;
mod input;
mod persist;
mod prepare;
mod promotion;
mod source;
mod validation_tools;

pub(super) use apply::apply;
pub(super) use assessment::member_ready;
use assessment::{
    unauthorized_missing_targets, validate_after_selectors, validate_recommendations,
};
pub(super) use drain_promotion::{
    approval_disqualification, approved_by_drain, held_classification,
};
use input::{
    action_failed, requested_workspace_root, required_string, required_string_array, string_array,
    string_array_value,
};
pub(super) use prepare::prepare;
pub(super) use promotion::{
    PromotionFindings, auto_approval_opted_out, promotion_findings, recommendation_has_evidence,
};
pub(crate) use source::requested_base_branch;

/// Field carrying the deterministic validation-tool feasibility findings, on
/// both a prepared task snapshot and the assessment apply reports for it
/// [ORB-11980].
pub(super) const VALIDATION_TOOL_WARNINGS: &str = "validation_tool_warnings";

/// Prepared-snapshot fields carrying the task's durable context creation
/// grant: the exact selectors it authorizes and the identity of the record.
pub(super) const CONTEXT_CREATION_SELECTORS: &str = "context_creation_selectors";
pub(super) const CONTEXT_CREATION_IDENTITY: &str = "context_creation_identity";

/// Assessment fields apply attaches: granted creation targets the pilot
/// omitted and apply kept, and dropped missing selectors no grant covers,
/// each naming the operator reauthorization path.
pub(super) const CONTEXT_CREATION_RETAINED: &str = "context_creation_retained";
pub(super) const CONTEXT_REAUTHORIZATION_REQUIRED: &str = "context_reauthorization_required";

#[cfg(test)]
mod tests;
