//! Host-local scheduler state for the routines feature [ORB-10021].
//!
//! All rows live in the host-global store database (`~/.orbit/orbit.db`) and
//! are never synced between hosts (ADR-0208): routine *definitions* converge
//! via git; fires, pauses, and the sweep lock stay local. Three tables:
//!
//! - `routine_cursors` — per routine: when this host first observed it
//!   (the baseline; a routine never fires for slots that predate its first
//!   observation) and the last scheduled slot consumed.
//! - `routine_fires` — one row per fire attempt, keyed on the idempotency
//!   triple (routine name, scheduled slot, attempt). Recording the intent
//!   and advancing the cursor happen in one transaction, so a slot can
//!   never double-fire even across crashed sweeps.
//! - `routine_pauses` — host-local suppressions written by
//!   `orbit routine pause`, invisible to git.
//!
//! The sweep advisory lock is a `flock(2)` file lock, not a table: the OS
//! releases it on process death, so a crashed sweep never wedges the next
//! one (see `try_acquire_routine_sweep_lock`).

mod backend;
mod cursor;
mod fires;
mod lock;
mod pause;

pub use crate::contracts::{
    RoutineCursor, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutinePauseRecord,
};

pub use lock::try_acquire_routine_sweep_lock;
