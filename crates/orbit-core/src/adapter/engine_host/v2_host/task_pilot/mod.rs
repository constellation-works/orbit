//! Deterministic support actions for the task-pilot workflow [ORB-10510].
//!
//! The agent leg only proposes task metadata. These actions own discovery,
//! partitioning and canonical selector validation. Ordinarily the sole write
//! is persisting assessed complexity and replacing `context_files` on the exact
//! tasks prepared for the run, with the assessment audit and replay receipt in
//! the same task-bundle commit. A CI-failure sweep may additionally request
//! explicit admission: after the selectors and every recommendation validate,
//! this boundary promotes only a current, warning-free repair from `proposed`
//! to `backlog`.

mod apply;
mod assessment;
mod attachment_budget;
mod input;
mod persist;
mod prepare;
mod source;
#[cfg(test)]
mod tests;
mod validation_tools;

pub(super) use apply::apply;
pub(super) use assessment::member_ready;
use assessment::{validate_after_selectors, validate_recommendations};
use input::{
    action_failed, requested_workspace_root, required_string, required_string_array, string_array,
    string_array_value,
};
#[cfg(test)]
pub(super) use persist::inject_concurrent_edit_before_locked_apply;
#[cfg(test)]
pub(super) use persist::inject_concurrent_edit_before_status_retry;
pub(super) use prepare::prepare;
pub(crate) use source::requested_base_branch;

/// Field carrying the deterministic validation-tool feasibility findings, on
/// both a prepared task snapshot and the assessment apply reports for it
/// [ORB-11980].
pub(super) const VALIDATION_TOOL_WARNINGS: &str = "validation_tool_warnings";
