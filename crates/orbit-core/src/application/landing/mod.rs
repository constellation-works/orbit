//! The owner's landing consumer [ORB-12499]: durable dispatch of authorized
//! handoffs and the trusted seam the landing activity settles through.
//!
//! # Why a job, and why dispatched from the outbox
//!
//! Accepting a completion-authorized handoff — or approving a review-only one —
//! records a durable landing-start request in the owner's coordination store.
//! This module is what consumes those requests: it reserves one landing attempt
//! per handoff and submits the owner-local [`LANDING_JOB`] that carries it.
//! Dispatch happens at the moment authority is recorded and again on demand, so
//! landing never waits for a drain, a ship sweep or any schedule. A request that
//! outlives its process stays pending and is picked up by the next dispatch pass,
//! which is the recovery path for a crash between the two.
//!
//! # What deduplicates work
//!
//! Handoff identity. The attempt row is keyed by handoff, the job's action key
//! is derived from handoff plus attempt number, and a handoff whose attempt has
//! already merged is refused rather than dispatched again. A live owner job is
//! left alone. Dispatch observes its stored state without reconciling process
//! liveness; opening a successor for an attached dispatched attempt requires
//! the store to prove that exact owner job is terminal and retain its stop in
//! attempt history. Missing run evidence never proves stoppage.
//!
//! # What this module never does
//!
//! It does not merge, observe a provider, or decide that a candidate landed.
//! Those belong to the landing activity (which reads real external state) and to
//! the coordination store (which rechecks the current authorization, the exact
//! candidate and the digest-pinned validation evidence inside the transaction

mod attempts;
mod context;
mod dispatch;
mod retry;

/// The owner-local job that lands one authorized handoff.
pub const LANDING_JOB: &str = "task_landing_pipeline";

pub use dispatch::LandingDispatch;
pub(crate) use dispatch::dispatch_recorded_authority;

#[cfg(feature = "test-support")]
pub use retry::retry_reservation_hook::with_landing_retry_reservation_hook;
