//! Refusal errors and the remedies they name.

use std::time::Duration;

use super::QUIESCE_TIMEOUT_ENV;
use super::handoff::HandoverCandidate;
use super::registry::{ParticipantRecord, ParticipantRole, PendingSwitch};
use super::update::{Standing, standing};
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

/// A newer binary waited for live participants to yield to its breaking
/// migration, and they did not.
pub(super) const BREAKING_WAITING: &str = "a breaking migration is waiting";

/// An older binary met newer live participants it cannot run beside.
pub(super) const INCOMPATIBLE: &str = "is incompatible with the live Orbit processes";

/// Remedy for a command inside a managed activity that meets an upgrade.
const IN_ACTIVITY: &str = "This command runs inside an Orbit-managed activity, which never \
     starts or waits on a generation switch. Stop the step and report blocker kind \
     `upgrade_pending`: the run keeps its candidate and continues after the upgrade";

/// Remedy when admission was only busy with other ordinary startups.
const CONTENDED: &str = "Retry the command; nothing is upgrading. If startups on this host \
     routinely take this long, raise the admission wait";

pub(crate) fn refusal(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, QUIESCE)
}

pub(super) fn unwritable(detail: impl std::fmt::Display) -> OrbitError {
    refused(detail, UNWRITABLE)
}

/// A command inside a managed activity met an upgrade it may not wait for.
pub(super) fn upgrade_pending(detail: impl std::fmt::Display) -> OrbitError {
    refused(
        format!("{} {detail}", orbit_types::workflow::UPGRADE_PENDING_MARKER),
        IN_ACTIVITY,
    )
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
        "{BREAKING_WAITING} ({reason}), and these Orbit processes did not yield within {}s: {}",
        bound.as_secs(),
        describe_blockers(blockers)
    ))
}

const UNREGISTERED: &str = "processes that did not register (executable-generation-v1 \
     binaries, or sandboxed children that cannot write the Orbit root)";

/// Live participants keep an updater out: each named with its role and what
/// would end its hold. With none registered, the holder never registered.
pub(super) fn holders_refused(
    holders: &[ParticipantRecord],
    candidate: Option<&HandoverCandidate>,
) -> OrbitError {
    refusal(format!(
        "Orbit clients or commands are still running: {}",
        describe_holders(holders, candidate)
    ))
}

/// Short-lived participants did not finish within `bound`.
pub(super) fn holders_outlasted(
    bound: Duration,
    holders: &[ParticipantRecord],
    candidate: Option<&HandoverCandidate>,
) -> OrbitError {
    refusal(format!(
        "Orbit clients or commands are still running after waiting {}s for them to finish \
         ({QUIESCE_TIMEOUT_ENV}): {}",
        bound.as_secs(),
        describe_holders(holders, candidate)
    ))
}

fn describe_holders(
    holders: &[ParticipantRecord],
    candidate: Option<&HandoverCandidate>,
) -> String {
    if holders.is_empty() {
        return format!(
            "{UNREGISTERED} — they neither register a role nor hand over; retry once they \
             exit, or stop them through their owners"
        );
    }
    let mut listed = holders
        .iter()
        .map(|holder| format!("{holder} — {}", remedy(holder, candidate)))
        .collect::<Vec<_>>()
        .join("; ");
    listed.push_str(&format!("; and any {UNREGISTERED}"));
    listed
}

/// Processes admitted to hand over to an installed candidate still held the
/// generation after `bound`. The executable is already replaced, so this is
/// not an admission refusal: what is left is pinning and convergence.
pub(super) fn handover_outlasted(bound: Duration, holders: &[ParticipantRecord]) -> OrbitError {
    OrbitError::Execution(format!(
        "the candidate is installed, but these Orbit processes did not hand over to it within \
         {}s ({QUIESCE_TIMEOUT_ENV}): {}; nothing was pinned or converged. A session hands over \
         once its in-flight requests are answered. A re-run renames nothing, so no session hands \
         over to it: close the live MCP clients, then re-run the update to converge",
        bound.as_secs(),
        describe_blockers(holders)
    ))
}

/// What ends `holder`'s hold on the generation.
fn remedy(holder: &ParticipantRecord, candidate: Option<&HandoverCandidate>) -> String {
    let capability = holder.handover.as_deref();
    match (standing(holder, candidate), holder.role, capability) {
        (Standing::HandsOver, ..) => "hands over to the candidate after the rename".into(),
        (_, ParticipantRole::Command, _) => "let the command finish".into(),
        (_, ParticipantRole::Clock, _) => {
            "let the tick finish, or pause scheduled ticks with `orbit clock pause`".into()
        }
        (_, ParticipantRole::McpServe, Some(capability)) => match candidate {
            Some(_) => format!(
                "the candidate does not report the {capability} resume capability this session \
                 needs; close its MCP client"
            ),
            None => format!(
                "it hands over ({capability}) only to a candidate renamed over the executable, \
                 which `orbit update` (a newer release) and `orbit update --local-candidate` \
                 admit and then install; nothing is renamed here, so close its MCP client to \
                 proceed without one"
            ),
        },
        (_, ParticipantRole::McpServe, None) => {
            "this mcp serve cannot hand over (its stdin cannot be polled, it proxies a remote \
             host, or its build predates handover); close its MCP client"
                .into()
        }
        (_, ParticipantRole::McpListen, _) => {
            "the TCP listener is never handed over; stop it and start it again after the upgrade"
                .into()
        }
        (_, ParticipantRole::Dashboard, _) => {
            "stop the dashboard and start it again after the upgrade".into()
        }
        (_, ParticipantRole::Drain, _) => "let its run finish, or cancel it".into(),
    }
}

pub(super) fn describe_blockers(blockers: &[ParticipantRecord]) -> String {
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
