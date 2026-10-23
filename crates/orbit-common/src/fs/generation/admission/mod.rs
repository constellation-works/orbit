//! Joining an authority: compatibility admission, the v1 fallback and quiesce.

mod guard;
mod join;
mod legacy;
mod lock;
mod waiters;

#[cfg(test)]
mod tests;

pub use guard::{
    GenerationGuard, Participant, pending_switch_for_this_process, process_handover,
    process_participation,
};
pub use lock::quiesce_bound;
pub(super) use lock::{Admission, PROBE_SETTLE, admission};
