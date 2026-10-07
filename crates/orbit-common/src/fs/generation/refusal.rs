//! Refusal errors and the remedies they name.

use std::time::Duration;

use super::QUIESCE_TIMEOUT_ENV;
use super::registry::{ParticipantRecord, PendingSwitch};
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

/// Remedy when admission was only busy with other ordinary startups.
const CONTENDED: &str = "Retry the command; nothing is upgrading. If startups on this host \
     routinely take this long, raise the admission wait";

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

pub(super) fn switch_pending(switch: &PendingSwitch) -> String {
    format!(
        "{SWITCH_PENDING}: pid {} ({}) is waiting until {} to migrate to {}",
        switch.pid,
        switch.role,
        switch
            .deadline
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        switch.target
    )
}

/// An upgrade holds admission: `pending`, a switch waiting for the live
/// participants to yield, or else an update or takeover holding the
/// generation exclusively.
pub(super) fn upgrade_holds_admission(pending: Option<&PendingSwitch>) -> OrbitError {
    match pending {
        Some(switch) => refusal(format!(
            "an upgrade is pending ({})",
            switch_pending(switch)
        )),
        None => refusal(
            "an upgrade is in progress: an Orbit update or generation takeover holds admission",
        ),
    }
}

/// Admission stayed held past `wait` by other startups alone.
pub(super) fn contended(wait: Duration) -> OrbitError {
    refused(
        format!(
            "admission stayed contended by other starting Orbit processes for {wait:?}, \
             with no upgrade pending"
        ),
        &format!("{CONTENDED} ({QUIESCE_TIMEOUT_ENV}, in seconds)"),
    )
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
