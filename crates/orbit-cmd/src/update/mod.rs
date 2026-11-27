//! `orbit update` — install a published Orbit release and converge to it.
//!
//! The command is one linear pipeline with an explicit, idempotent order:
//! decide the target version, refuse a channel Orbit does not own, take the
//! update lock, re-read the installed version (resolving a replaced Linux
//! inode back to the live path), then stage and authenticate the archive,
//! confirm the staged executable reports the requested version, take
//! generation admission (admitting beside live processes that hand over to
//! that candidate), swap it in atomically, verify the installed path, pin
//! its generation once those processes have handed over, then let *that*
//! executable migrate `.orbit/` state, reconcile managed assets, and repoint
//! the host clock unit.
//!
//! Migration runs before managed-asset sync because a layout migration can
//! move the directories those assets live in; converging assets first would
//! write them into the shape the upgrade is about to leave behind. The clock
//! unit is converged last because it is host state rather than workspace
//! state: it is the one step that still runs outside a workspace, and
//! re-arming it against a workspace whose migration failed would only produce
//! a failing sweep every minute.
//!
//! Every stage before the swap fails with nothing changed. After the swap the
//! rule inverts: `.orbit/` may already be partly migrated, so recovery is
//! forward — re-running `orbit update` re-enters at the convergence steps,
//! which are the same idempotent operations the operator would run by hand.
//!
//! `--local-candidate` runs the same pipeline for an operator-built executable
//! identified by its digest and an operator-attested source commit instead of
//! a signed release version; see [`local_candidate`].

pub mod bundled_bwrap;
mod environment;
pub mod source;

pub use environment::{for_install_target, from_process, workspace_for_preflight};

pub use orbit_core::app::update::*;

#[cfg(test)]
mod tests;
