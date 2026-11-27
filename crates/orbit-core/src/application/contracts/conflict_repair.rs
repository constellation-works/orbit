//! Source-repair route classification for a conflicting pipeline publication.

use orbit_types::workflow::JobRunState;

/// How explicit source repair can resolve a conflicting pipeline publication
/// from the task's current status and source run state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictSourceRepairRoute {
    /// `inspect` returns `policy_digest: null`; `submit` takes `request_key`
    /// and `diagnosis` with `unmet_acceptance` and `policy_digest` unset.
    FieldFree,
    /// `inspect` returns a non-null `policy_digest`; `submit` requires
    /// `request_key`, `diagnosis`, `unmet_acceptance`, and `policy_digest`.
    ReworkRequired {
        /// Kept-inline final recovery left uncommitted edits in the
        /// retained checkout that `inspect` must adopt.
        adopt_retained_edits: bool,
    },
    /// Explicit source repair does not admit this source state in the task's
    /// current status.
    Unavailable {
        /// The publishing run's terminal state, when available.
        source_state: Option<JobRunState>,
    },
}
