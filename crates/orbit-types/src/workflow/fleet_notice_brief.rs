//! The fleet-notice brief a host builds for one agent prompt: the bounded
//! `fleet_notices` task-context value, marked `untrusted_data`, plus the
//! notice revisions it carried for the delivery audit record.

use serde_json::Value;

/// A host's answer for one prompt build of an activity that opts into
/// `fleet_notices` or declares a `notice_instruction`.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetNoticeBrief {
    /// The task context's `fleet_notices` value.
    pub block: Value,
    /// Each delivered notice as `id@revision`; empty for the `unavailable`
    /// marker.
    pub notices: Vec<String>,
    /// Open notices in scope the brief's item bound omitted.
    pub omitted: u64,
}
