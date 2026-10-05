//! Cross-process upgrade admission for one authority root.
//!
//! OS locks, not process discovery or expiring leases, define liveness. Never
//! unlink these files: replacing a locked inode would create a second authority.
//!
//! # Compatibility generations (`compatibility-generation-v2`)
//!
//! A generation is a [`CompatibilityIdentity`] — the store schema, workspace
//! layout and feature schema versions a binary was compiled with, and the
//! newest migrations older binaries could not keep reading or writing — not
//! the executable's digest. Every participant holds `.generation.lock` shared
//! for its lifetime. `.generation-compat.json` records the envelope of every
//! identity admitted since the authority last had no participant. A joiner
//! that finds the generation lock unheld replaces that envelope with its own
//! identity before the compatibility check. Dropping a guard does not clear
//! the file; the lock is what shows the authority is empty. Admission then
//! follows that envelope:
//!
//! - A newcomer compatible with that envelope joins as an ordinary writer (or
//!   reader) whatever its digest, and widens it. A rebuild or patch release
//!   with the same state versions, or one whose newer migrations are all
//!   [additive](../../../orbit_store/contracts/enum.MigrationCompatibility.html),
//!   runs — and migrates — beside the live processes.
//! - An incompatible *newer* writer (a pending breaking migration) records a
//!   [`PendingSwitch`] and waits, up to [`quiesce_bound`], for exclusive
//!   admission. Live participants observe the switch at their safe points and
//!   yield; a newcomer of any other identity is refused while it is pending.
//!   When the bound expires the refusal names every live participant by PID,
//!   role and start time.
//! - An incompatible *older* binary is refused outright: it may not displace
//!   newer participants.
//!
//! Participants register PID, role and start time under
//! `.generation-participants/`, each record held by its own lock.
//!
//! # Yielding and handing over
//!
//! Long-lived participants ([`ParticipantRole::is_long_lived`]) check
//! [`pending_switch_for_this_process`] at their safe points: `mcp serve`
//! between requests, the dashboard between lifecycle ticks, a pipeline worker
//! after each checkpointed step (recording its run interrupted, not failed).
//! Independently, when [`replaced_installation`] reports that the installed
//! executable no longer names their running image, `mcp serve`, the
//! dashboard and drain coordinators ask [`handover_target`] whether the
//! replacement speaks this contract and their [`RESUME_CAPABILITIES`] entry,
//! and [`reexec`] it at an idle boundary. The exec keeps the PID and releases
//! every lock (Orbit descriptors are close-on-exec), so the new image joins
//! like any newcomer.
//!
//! # Coexistence with `executable-generation-v1`
//!
//! `.generation.lock` keeps its v1 content (`1:<sha256>\n`), and every v2
//! participant holds it shared, so a v1 process still refuses to write, and a
//! v1 `orbit update` still refuses to replace the executable, while any v2
//! process is live. The v2 record is authoritative only while its
//! `record_digest` equals that v1 content: a v1 takeover rewrites the content,
//! which invalidates it. A v2 newcomer that finds a v1-owned record falls back
//! to v1 rules — exact digest, or exclusive takeover, otherwise refused
//! immediately (v1 processes never observe a pending switch, so waiting for
//! them would only stall). [`GenerationGuard::acquire`] pins an exact digest
//! under v1 rules alone, as a v1 process would.

use std::time::Duration;

mod admission;
mod clock_hold;
mod handoff;
mod identity;
mod image;
mod image_digest;
mod paths;
mod records;
mod refusal;
mod registry;
mod update;

pub use admission::{
    GenerationGuard, Participant, pending_switch_for_this_process, process_participation,
    quiesce_bound,
};
pub use clock_hold::{
    finish_clock_generation_hold, is_clock_generation_hold, record_clock_generation_hold,
};
pub use handoff::{
    RESUME_CAPABILITIES, RESUME_DRAIN_ADOPT, RESUME_MCP_STDIO, candidate_supports, handover_target,
    reexec, replaced_installation,
};
pub use identity::{Access, CompatibilityIdentity, LedgerCompatibility};
pub use image::{executable_generation, process_generation};
pub use paths::authority_root;
pub use registry::{ParticipantRecord, ParticipantRole, PendingSwitch, pending_switch};
pub use update::GenerationUpdate;

/// Admission protocol this binary implements.
pub const GENERATION_CONTRACT: &str = "compatibility-generation-v2";

/// The executable-digest protocol every v2 binary still honours toward v1
/// participants and v1 updaters.
pub const LEGACY_GENERATION_CONTRACT: &str = "executable-generation-v1";

/// Environment variable bounding how long a breaking upgrade waits for live
/// participants to yield, in seconds.
pub const QUIESCE_TIMEOUT_ENV: &str = "ORBIT_UPGRADE_QUIESCE_SECS";

/// Default [`QUIESCE_TIMEOUT_ENV`].
pub const DEFAULT_QUIESCE_TIMEOUT: Duration = Duration::from_secs(120);
