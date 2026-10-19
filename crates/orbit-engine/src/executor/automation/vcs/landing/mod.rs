//! The owner's landing step for an authorized handoff [ORB-12499].
//!
//! A follower executes, validates and publishes a candidate; it never merges.
//! The owner's durable landing job runs this activity, which is the only place
//! the external merge actually happens. Its contract with the owner store is:
//!
//! 1. **Observe, never assume.** Every fact this activity reports — the pull
//!    request's head, base and merged state, the candidate and base objects,
//!    the local landing ref — is read from the provider or the owner checkout
//!    here. Nothing is copied out of the worker's handoff payload, and a fact
//!    that cannot be read is a refusal rather than a pass. Fields the owner
//!    holds but cannot independently observe (the repository name on a
//!    checkout without a remote) are carried from the accepted candidate and
//!    named as such in the evidence.
//! 2. **Intent before the call.** A merge intent is persisted before the first
//!    external mutation, so a lost reply or a crash leaves recorded
//!    uncertainty. The next attempt reconciles that intent against the
//!    provider's actual state (or the local target ref) before it retries
//!    anything, and the owner store refuses reassignment until it does.
//! 3. **Stop rather than repair.** A changed head or base, a conflict, an
//!    unsatisfied branch protection or a check budget that runs out stops the
//!    attempt with durable evidence. Repairing the candidate needs fresh
//!    validation and a new handoff; this activity never rebases, force-pushes
//!    or merges around a gate. A stop on a conflicting or stale base is
//!    recorded as repairable, and the owner store hands that candidate to one
//!    automatic repair leaf that does exactly that [ORB-14261]. A failed
//!    hosted check is classified first ([`checks`]): a failure
//!    inherited from the base updates the branch onto the current base once,
//!    and one that still fails there on a base tip where the same job passes
//!    stops for the one automatic repair; a transient one is rerun once, and
//!    one the candidate caused stops for the same single repair [ORB-15956]
//!    [ORB-16322]. Once that repair is spent, a candidate-caused failure
//!    stops for a requeue: the owner store ends the claim and returns the
//!    task to the backlog with the candidate and the failure evidence, so its
//!    next run repairs that candidate [ORB-16890].
//! 4. **Settle every run.** A provider transport failure (a TLS or request
//!    timeout, a reset connection, a 5xx) that interrupts a pass before it
//!    records a merge intent or requests a check recovery is retried
//!    against the same handoff, a bounded number of times with backoff,
//!    from the owner's current state. A run that still fails, for that or
//!    any other reason, records a stop with its error, so the attempt never
//!    stays dispatched behind a finished run. The one exception is a merge
//!    intent this owner sent and could not resolve: that attempt keeps its
//!    recorded uncertainty, which the next attempt reconciles first.
//!
//! The guarded `review -> done` transition is the owner store's, behind its own
//! recheck of the current authorization and the digest-pinned validation
//! evidence. This activity only supplies the merge evidence that permits it.

pub(super) mod checks;
mod intent;
mod local;
mod observe;
mod pull_request;
mod record;
mod run;

pub(in crate::executor::automation) use run::handoff_land;
