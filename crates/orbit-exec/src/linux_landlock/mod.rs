//! Linux Landlock read confinement for activity-scoped `proc.spawn`.
//!
//! Guessing which argv strings look like paths cannot see what an allowed
//! program will do with them. `git`, `bash`, and `python3` are all on shipped
//! activity allowlists, and every one of them will interpret text supplied on
//! its command line — `git -c alias.x='!cat /etc/shadow' x` reaches the host
//! without a single path-shaped argument. The read boundary therefore has to
//! live where the child does, not in the request.
//!
//! This module compiles the activity's resolved read profile into a Landlock
//! ruleset and applies it between `fork` and `exec`, so the child and every
//! descendant it spawns inherit it. The request-time argv check remains, but
//! only as an early, explainable deny for `git -C /etc`; it is no longer the
//! security boundary.
//!
//! # What the ruleset covers
//! Read and execute access (`READ_FILE`, `READ_DIR`, `EXECUTE`) plus
//! `REFER`, which governs moving a file between directories. For the
//! activity-scoped profile, writes are not handled here: agent write
//! confinement belongs to the Bubblewrap mount namespace in
//! [`crate::linux_sandbox`], and handling writes in two places would leave
//! two answers to one question.
//!
//! A plugin backend has no Bubblewrap wrapper and no agent profile: its
//! boundary is the explicit read and write roots the operator granted
//! ([`LandlockBoundary`]). That ruleset additionally takes over every
//! write-side right, so a write outside the granted roots is refused by the
//! kernel, and on ABI 4 refuses TCP for a plugin whose manifest declares
//! `network: none` (design `docs/design/plugins/1_scope.md` §4.3).
//!
//! `REFER` is handled because Landlock refuses a rename or link that would
//! give a file *more* access at its destination. Without it, a child could
//! move a denied file into a fully readable sibling directory and read it
//! there. See [`workspace`] for how denied paths are carved out.
//!
//! # What the ruleset does not cover
//! Landlock rules bind to inodes, so the ruleset cannot single out a *name*
//! that does not exist yet. The compiler answers that by asking what the
//! profile's exclusions can name rather than what they currently match: a
//! directory a bounded exclusion reaches into is granted list-only, so a name
//! created there afterwards has no readable ancestor. An exclusion whose reach
//! crosses directories (`**/.env`) reaches every directory in the workspace,
//! and carving that out would leave the run unable to read the files it
//! produces itself. Those rules are reported on
//! [`LandlockReadBoundary::unenforced_exclusions`](boundary::LandlockReadBoundary::unenforced_exclusions)
//! instead of being enforced or quietly dropped. [`workspace`] carries the
//! full reasoning.
//!
//! The boundary also governs acquisition rather than naming: an inode the
//! ruleset already grants keeps that grant through a rename or hard link into
//! a denied name, and bytes already read, mapped, or held on an open
//! descriptor cannot be withdrawn by any later rule.

mod boundary;
mod grants;
mod host;
mod probe;
mod spawn;
mod workspace;

#[cfg(target_os = "linux")]
mod ruleset;

pub use boundary::{
    LandlockBoundary, linux_landlock_boundary_grants, linux_landlock_read_boundary,
};
pub use grants::{LandlockPathGrant, grants_read};
pub use probe::{NETWORK_LANDLOCK_ABI, WRITE_LANDLOCK_ABI, probe_landlock};
pub use spawn::{spawn_under_linux_landlock, spawn_under_linux_landlock_boundary};

#[cfg(test)]
#[path = "tests/mod.rs"]
mod tests;
