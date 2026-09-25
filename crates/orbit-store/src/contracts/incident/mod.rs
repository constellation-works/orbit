//! Failure-incident grouping over raw audit events [ORB-10871].
//!
//! A raw failed audit event is forensic evidence, not an incident. One burst
//! of the same refusal repeated hundreds of times is one problem, and one
//! failed pipeline run that propagates its failure up through its enclosing
//! steps is one root cause with a chain hanging off it. Counting the raw rows
//! as independent failures overstates both.
//!
//! This module derives an incident view *on top of* the audit rows without
//! consuming or rewriting them: every incident carries its raw `event_count`
//! and the ids of the rows it collapsed, so the raw Audit view and any export
//! stay the authority on evidence.
//!
//! The contract is deliberately project-agnostic — it reads only durable
//! audit columns (status, role, tool/command, error message, run/task ids,
//! timestamps). No tool name, agent, workspace, or task id is special-cased.
//!
//! Grouping runs in three passes:
//!
//! 1. **Classify** each failed row as [`FailureClass::Denied`],
//!    [`FailureClass::Expected`], [`FailureClass::Diagnostic`], or
//!    [`FailureClass::Unexpected`] so a policy refusal, caller/input negative,
//!    and lifecycle diagnostic stay distinguishable from a genuine unexpected
//!    failure.
//! 2. **Cluster** rows by `(run scope, signature)`, where the signature is
//!    `class | role | surface | normalized message`. Volatile tokens (paths,
//!    numbers, ids, timestamps, hashes, quoted literals) are replaced with
//!    placeholders, so a repeated failure whose only difference is its operand
//!    collapses into one cluster.
//! 3. **Collapse cascades** within a single job run: clusters of the same
//!    class whose time ranges are within [`CASCADE_WINDOW_SECS`] of each other
//!    are one incident, rooted at the earliest cluster, with the later ones
//!    recorded as its propagation chain. A failure later in the same run,
//!    beyond that window, stays an independent incident.
//! 4. **Collapse cited-run cascades** across job runs: a later incident whose
//!    raw message names another incident's `job_run_id` (parent/child guard
//!    copies) folds onto that cited root. Matching is token ∩ known run ids,
//!    not a special-cased tool or activity name.

mod classify;
mod grouping;
mod signature;
mod types;

pub use classify::{
    classify, has_tool_identity, is_failure_only_diagnostic_surface, is_lifecycle_diagnostic,
};
pub use grouping::{build_report, group_failure_incidents};
pub use signature::{normalize_message, signature_for};
pub use types::{
    CASCADE_WINDOW_SECS, DEFAULT_SCAN_LIMIT, FAILURE_ONLY_DIAGNOSTIC_SURFACES, FailureClass,
    FailureIncident, FailureIncidentQuery, FailureIncidentReport, IncidentEventRef,
    JOB_RUN_LIFECYCLE_CATEGORY, JOB_RUN_LIFECYCLE_LABEL, LIFECYCLE_DIAGNOSTIC_LABEL,
    PropagationLink,
};
