//! `pull_refill`: one iteration of a follower's pull drain [ORB-13625].
//!
//! Each call carries every earlier admission forward — retry, bind, launch,
//! settle — and, while the window is open and the owner would admit this
//! executor, tops the free slots up with new pull requests. Three consecutive
//! failed passes latch a durable degraded warning and stop new admissions.
//! Protocol skew fails immediately with its typed code and preserves the
//! durable settlement outbox. Other settlement retries keep flowing. If run state cannot be read or recorded,
//! the activity fails visibly rather than retrying without health evidence.
//!
//! Each request declares the crews this window can run [ORB-13941]: the
//! provider preflight taken on the window's first pass, minus every crew a
//! claimed leaf has since found unusable, and within the drain's
//! `--allow-crew` restriction when it has one [ORB-14174]. The owner skips a
//! task whose crew is not among them, so a follower never burns a claim it
//! cannot run. The owner's before-PR or before-landing reviewer is checked
//! against the window without that restriction. Each request also declares
//! this host's OS, and the owner skips a task whose `os:` tags that OS does
//! not satisfy.
//!
//! A drain submitted without a window (`for_seconds` zero) is authorized for
//! one admission pass [ORB-14174]. Its window is expired from the start, so
//! the pass is not read off the window: the first pass that finds no stop or
//! cancel takes it, recording it in run state before probing or requesting,
//! and requests up to the free slots. Every later pass — the same run's,
//! a retry's, or a resumed run's — finds it taken and only settles. A pass
//! held by a throttle, a shutdown or an owner refusal is still that pass.
//! A timed window that has expired never gains one.
//!
//! A stop or cancel recorded while a pass is requesting ends it before the
//! next request.
//!
//! The free slots are the store's allocation reading [ORB-15194]: a claimed
//! leaf in its before-landing review still holds its slot until it settles,
//! but admits a replacement beside it, up to `max_active_leaf_runs` such
//! leaves, so a one-to-two-hour review does not idle the slot.
//!
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered: by the leaf's own bound worker as it ends, by any later settle
//! pass, or by this drain's next iteration, whichever gets there first.
//!
//! Each pass reads the window from the run's own state rather than trusting
//! the answer the job forwarded, so an `orbit run extend` accepted after that
//! answer reopens requests [ORB-15417]. The pass that would end the drain
//! records the close in the transaction that rereads the deadline, the one an
//! extension checks: an extension accepted first keeps the drain admitting,
//! and one requested after the close is refused.
//!
//! A settlement the owner refuses while it still holds the claim holds new
//! requests too [ORB-13979]: the owner answered, and will answer the same
//! until an operator changes it. The refusal is recorded on the admission,
//! logged once and listed by `orbit run show`; delivery backs off (doubling to
//! at most 15 minutes) instead of asking on every pass, and is not a failed
//! pass. Requests resume on the pass after the owner accepts it.
//!
//! Sustained host resource pressure holds requests the same way a pending
//! host shutdown does [ORB-13901]: the pass requests nothing, keeps settling,
//! records the throttle on the drain's last pass and polls for recovery. The
//! pressure is this follower's own, because a claimed leaf runs here; the
//! owner's throttle gates only the owner's own drain and discovery, and its
//! pull admission never reads it [ORB-15637].
//!
//! Every other reason a pass requests nothing although its window is open —
//! an owner probe refusal, a refused settlement, the failure breaker, a
//! shutdown inside `machine.host_shutdown_hold_minutes`
//! (`host_shutdown_scheduled`), or no runnable crew — is recorded on the
//! drain's last pass as `admission_refusal`, so `orbit run show` and
//! readiness name it [ORB-15637]. A schedule further away is reported and
//! does not hold requests. Host pressure keeps its own field and leaves
//! `admission_refusal` unset.
//!
//! The failure breaker opens after three claims in a row settle as failures
//! and half-opens after `breaker_cooldown_seconds` to admit one probe claim
//! ([`super::drain::settle_breaker`]) [ORB-15639]. Each pass reads it with one
//! clock reading, records it in run state as `pull_breaker`, and asks it again
//! before every request, after reconciliation, so a leaf the pass just
//! settled as failed already counts.
//!
//! Under `machine.auto_concurrency` `enforce`, each request also carries the
//! work this host's load forecast has room for, computed under the host
//! admission lock just before it, and the owner admits only a task inside it.
//! When nothing fits the pass requests nothing and records
//! `forecast_no_headroom`. Pending request capacity is reserved in the fit's
//! transaction, refreshed before carried re-sends, and atomically exchanged
//! for precise task capacity before leaf creation. Shadow mode records the list as
//! `pull_admission_fit` and requests as with `off`; with `off`, or before the
//! host has resource profiles, requests carry no list. Every computed fit
//! records one accuracy forecast with the request sent/no-headroom decision
//! and a returned claim priced as its reservation.
//!
//! Each pass sends at most the drain's live ceiling of requests, an operator's
//! `orbit run concurrency` included, not the ceiling it was submitted with.
//!
//! A graceful `orbit run cancel` puts the drain into its cancelling mode:
//! each pass then requests nothing, releases every admission that never
//! launched back to the owner's backlog, and waits for the launched leaves to
//! finish and settle. The pass that finds nothing left unsettled ends the
//! drain `cancelled`. Cancellation is read again after orphaned leaves are
//! reconciled, so a request recorded during that work is observed before any
//! probe, pull request, or leaf launch in the same pass. If that state cannot
//! be read, the entire pass waits and fails before probing or advancing
//! pending requests: neither new work nor an earlier unanswered request may
//! be admitted without knowing whether the drain is cancelling.

mod cancel;
mod crews;
mod input;
mod owner;
mod pass;
mod record;

/// The job that runs this action. Recorded in each request's run context, so
/// the owner's claim names the drain that holds it.
pub(crate) const PULL_DRAIN_JOB_NAME: &str = "workspace_pull_pipeline";

pub(crate) use pass::pull_refill;
