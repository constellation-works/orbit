//! Refusal errors and the remedies they name.

use std::time::Duration;

use super::registry::ParticipantRecord;
use crate::OrbitError;

const QUIESCE: &str = "Quiesce the existing Orbit processes through their owning clients, \
     then retry. Do not delete admission files or replay a mutation whose reply was lost";

/// Remedy for a record this process can read but never rewrite.
const UNWRITABLE: &str = "Run the recorded generation, or retry where the Orbit root is writable. \
     A read-only mount, or a sandbox that denies writes under this root, \
     cannot record a takeover";

pub(super) const WRITES_WHILE_FOREIGN: &str = "another executable generation is still running \
     (this command writes; read-only commands are admitted when the store schema matches)";

pub(super) const SWITCH_PENDING: &str = "a generation switch is pending";

pub(crate) fn refusal(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, QUIESCE)
}

pub(super) fn unwritable(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, UNWRITABLE)
}

fn refused(detail: impl std::fmt::Display, remedy: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "upgrade admission refused: {detail}; leave the installation and stores unchanged. \
         {remedy}"
    ))
}

pub(super) fn quiesce_timeout(
    reason: &str,
    bound: Duration,
    blockers: &[ParticipantRecord],
) -> OrbitError {
    refusal(format!(
        "a breaking migration is waiting ({reason}), and these Orbit processes did not yield \
         within {}s: {}",
        bound.as_secs(),
        describe_blockers(blockers)
    ))
}

pub(super) fn describe_blockers(blockers: &[ParticipantRecord]) -> String {
    const UNREGISTERED: &str = "processes that did not register (executable-generation-v1 \
         binaries, or sandboxed children that cannot write the Orbit root)";
    if blockers.is_empty() {
        return UNREGISTERED.to_string();
    }
    let mut listed = blockers
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    listed.push_str(&format!(", and any {UNREGISTERED}"));
    listed
}
