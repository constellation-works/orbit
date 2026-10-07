//! Pull refill loop. The owner transport and the leaf launcher are injected;
//! the loop owns only the durable checkpoint discipline between them.
use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_common::text::floor_char_boundary;
use orbit_store::contracts::{
    AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimEvidence, ClaimFailure,
    ClaimFinalRecovery, ClaimMutation, JobRunStoreBackend, LocalPullAdmission, LocalPullMutation,
    LocalPullPhase, ProviderUnavailable, PullDestination, SettlementRefusal,
};
use orbit_types::workflow::{
    BASELINE_RED_MARKER, BaselineRedHold, ClaimFailureClass, FORGE_UNAVAILABLE_MARKER,
    FinalRecoveryDecision, ForgeUnavailableHold, JobRunState, OWNER_ROUTE_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_MARKER, PROVIDER_UNAVAILABLE_MARKER, PipelineState, ReviewEvidenceHold,
    TRANSIENT_FAILURE_MARKER, VALIDATION_ENVIRONMENT_MARKER, is_baseline_red_failure,
};

use super::candidate::{
    SYNC_BASE_STEP, candidate_note, first_incomplete_step, preserved_candidate,
};
use crate::application::distributed::{
    is_owner_refusal, is_owner_transport_failure, settlement_refusal_backoff,
};
use crate::application::job::pipeline::WorkerLaunchError;

/// Trusted owner transport, supplied by runtime composition. Implementations
/// must check current claim/run/phase on bind and settlement; replaying a receipt
/// never supplies execution authority. There is no local fallback.
pub(crate) trait PullPeer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError>;
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    /// Read-only receipt reconciliation for one of this executor's request
    /// IDs. It never admits, and it does not reapply the version, ship or
    /// policy checks a replay of the request itself would.
    fn lookup(
        &self,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError>;
}

/// A launcher takes the existing bound run, never submits a replacement.
/// Success means that launch was acknowledged, not that execution completed.
pub(crate) trait PullLauncher {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), WorkerLaunchError>;
    /// Cancel a bound leaf that was never launched, so it can never start.
    /// Also used after a confirmed pre-spawn launch failure. A leaf that
    /// has started is refused, so a release cannot permit concurrent work.
    fn cancel_queued(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
}

/// How far a settle-only pass may carry an admission [ORB-13663].
///
/// A settle-only pass never requests, binds for execution, or launches work,
/// so any follower process may run one — a new drain, `orbit run cancel`,
/// `orbit run auto --stop`, or the leaf's own worker as it terminalizes. The
/// admission record is the outbox: whatever a pass cannot deliver stays
/// recorded for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettleScope {
    /// Deliver recorded settlements, and record the failure a terminal leaf
    /// implies. Admissions still waiting on their drain are left to it.
    Deliver,
    /// No live drain will carry this admission forward — its own drain ended
    /// and none pulls from its owner: release what was never launched back to
    /// the owner, cancelling a queued leaf, so the task returns to the backlog
    /// and the owner is never left holding a claim no follower process is
    /// responsible for. Nothing ran, so nothing is failed. Live leaves are
    /// left running; they settle themselves when they terminalize.
    Abandon,
    /// The pass of a drain that is being cancelled gracefully, over its own
    /// owner's admissions: release everything unlaunched as
    /// [`Self::Abandon`] does, without asking whether a drain is live (this
    /// one is, and it is the one giving the work back), and withdraw a
    /// request the owner holds no receipt for, so the drain can finish. Live
    /// leaves are waited for.
    Cancel,
}

/// When a pass delivers a settlement the owner refused while still holding
/// its claim [ORB-13979]. Such a refusal is an answer that repeats until an
/// operator changes the owner, so automatic passes back off instead of asking
/// on every one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusedDelivery {
    /// Once its backoff has elapsed: a drain pass, the clock sweep, a leaf's
    /// own worker.
    WhenDue,
    /// Now, whatever the backoff: an operator's settle-only pass, so an
    /// operator who fixed the owner need not wait it out.
    Now,
}

/// A [`PullPeer`] that stops calling the owner after its first transport
/// failure. A hung owner costs one blocking call (up to the routed delivery
/// timeout) per pass, not one per admission; later calls fail fast with
/// [`OrbitError::OwnerUnavailable`] and the local-only phases still run.
struct FencedPeer<'a> {
    inner: &'a dyn PullPeer,
    failed: RefCell<Option<String>>,
}

impl FencedPeer<'_> {
    fn call<T>(&self, call: impl FnOnce() -> Result<T, OrbitError>) -> Result<T, OrbitError> {
        if let Some(first) = self.failed.borrow().as_deref() {
            return Err(OrbitError::OwnerUnavailable(format!(
                "not contacted again this pass; the owner already failed: {first}"
            )));
        }
        let result = call();
        if let Err(error) = &result
            && is_owner_transport_failure(error)
        {
            *self.failed.borrow_mut() = Some(error.to_string());
        }
        result
    }
}

impl PullPeer for FencedPeer<'_> {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError> {
        self.call(|| self.inner.request(destination, request))
    }
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.call(|| self.inner.bind(admission))
    }
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError> {
        self.call(|| self.inner.settle(admission))
    }
    fn lookup(
        &self,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<AdmissionLookup, OrbitError> {
        self.call(|| self.inner.lookup(destination, request_id))
    }
}

/// Consecutive claims a drain settled as failures, with no handoff between
/// them, after which it stops requesting new work.
///
/// A leaf that fails fast frees its slot within seconds, so without a breaker
/// a systemic executor fault — a missing credential, a broken toolchain, an
/// incompatible owner — would claim and block the owner's backlog one task per
/// poll. Settlement of work already running is unaffected, and an operator
/// resets the breaker by starting a new drain.
pub(crate) const CONSECUTIVE_FAILURE_BREAKER: usize = 3;

/// What one [`PullDrain::refill_pass`] did: the claims it admitted, the
/// error that ended it, if one did, and the owner's last answer. The three are
/// independent — a pass can admit claims and then fail on a later one.
pub(crate) struct RefillPass {
    pub(crate) admitted: usize,
    pub(crate) error: Option<OrbitError>,
    /// The last receipt the owner returned to a request this pass sent or
    /// retried [ORB-14475]: its diagnostics are what the owner kept off this
    /// executor. `None` when the pass sent no request, or ended in an error
    /// before one was answered.
    pub(crate) answer: Option<Box<AdmissionReceipt>>,
}

/// How far one admission carried, and what that leaves the pass free to do.
enum Reconciled {
    /// Nothing the admission owes holds new requests back.
    Open,
    /// A settlement the owner has not accepted holds new requests back.
    Held,
    /// The owner answered a request idle: nothing ready for this executor.
    Idle(Box<AdmissionReceipt>),
}

/// How the owner answered a bind.
enum Bind {
    Bound(Box<LocalPullAdmission>),
    Refused(OrbitError),
}

pub(crate) struct PullDrain<'a> {
    pub(crate) jobs: &'a dyn JobRunStoreBackend,
    pub(crate) peer: &'a dyn PullPeer,
    pub(crate) launcher: &'a dyn PullLauncher,
    pub(crate) refused_delivery: RefusedDelivery,
}

impl PullDrain<'_> {
    /// One bounded refill. Each new ID is made durable before the request goes
    /// on the wire. An idle response ends the whole pass, regardless of free
    /// slots. Reconciliation errors prevent all fresh admission.
    ///
    /// The failure breaker is checked after reconciliation, because settling
    /// a leaf that has just failed can be what opens it: that pass must not
    /// request replacements.
    ///
    /// The count of claims admitted survives an error that ends the pass
    /// later: those leaves are already running, and the caller's next wait
    /// depends on it.
    ///
    /// `template` is built after reconciliation, so what this pass just
    /// settled — a leaf whose provider proved unusable, say — already shapes
    /// the requests it sends [ORB-13941]. `None` requests nothing.
    ///
    /// `admitting` is asked before each new request, so an operator stop or
    /// cancel recorded while the pass runs ends it before the next one
    /// [ORB-14174]; an error from it ends the pass too.
    pub(crate) fn refill_pass(
        &self,
        destination: &PullDestination,
        template: &dyn Fn() -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        ceiling: usize,
    ) -> RefillPass {
        let mut admitted = 0;
        let mut answer = None;
        let error = self
            .refill_into(
                destination,
                template,
                admitting,
                ceiling,
                &mut admitted,
                &mut answer,
            )
            .err();
        RefillPass {
            admitted,
            error,
            answer,
        }
    }

    /// [`Self::refill_pass`] as a `Result`, for fixtures that only care
    /// whether the pass completed.
    #[cfg(test)]
    pub(crate) fn refill(
        &self,
        destination: &PullDestination,
        template: &AdmissionRequest,
        ceiling: usize,
    ) -> Result<usize, OrbitError> {
        let pass = self.refill_pass(
            destination,
            &|| Ok(Some(template.clone())),
            &|| Ok(true),
            ceiling,
        );
        match pass.error {
            Some(error) => Err(error),
            None => Ok(pass.admitted),
        }
    }

    fn refill_into(
        &self,
        destination: &PullDestination,
        template: &dyn Fn() -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        ceiling: usize,
        admitted: &mut usize,
        answer: &mut Option<Box<AdmissionReceipt>>,
    ) -> Result<(), OrbitError> {
        match self.reconcile_pending_answer(destination)? {
            Reconciled::Open => {}
            Reconciled::Held => return Ok(()),
            Reconciled::Idle(receipt) => {
                *answer = Some(receipt);
                return Ok(());
            }
        }
        let Some(mut next) = template()? else {
            return Ok(());
        };
        if self.consecutive_failed_settlements(destination, &next.run_context.run_id)?
            >= CONSECUTIVE_FAILURE_BREAKER
        {
            return Ok(());
        }
        for slot in 0..ceiling {
            if !admitting()? {
                break;
            }
            // [ORB-14257] Each request after the first is built again: a leaf
            // this pass launched may already have released its claim and
            // excluded its crew, and must not be pulled straight back.
            if slot > 0 {
                let Some(fresh) = template()? else {
                    break;
                };
                next = fresh;
            }
            let template = &next;
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| {
                OrbitError::Execution(format!("allocate pull request identity: {error}"))
            })?;
            let mut request = template.clone();
            request.request_id = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let Some(record) = self
                .jobs
                .allocate_pull_request(destination, &request, ceiling)?
            else {
                break;
            };
            match self.reconcile(record)? {
                Reconciled::Open => *admitted += 1,
                Reconciled::Held => break,
                Reconciled::Idle(receipt) => {
                    *answer = Some(receipt);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Carry every earlier admission for `destination` forward — retry an
    /// unanswered request, bind, launch, and settle — without allocating
    /// anything new. A drain whose window has closed, or whose owner currently
    /// refuses new work, still runs this so a finished leaf's settlement
    /// reaches the owner.
    ///
    /// Returns false when an earlier unanswered request turned out idle: the
    /// owner has nothing ready, so this pass allocates nothing new.
    ///
    /// One admission that cannot move forward does not hold the others back:
    /// every record is carried as far as it goes, and the first error is
    /// returned afterwards, so it still prevents fresh admission this pass.
    ///
    /// The owner is the exception: after its first transport failure the pass
    /// stops calling it, so a hung owner costs one blocking call rather than
    /// one per admission. Records still advance through their local-only
    /// phases, such as recording a terminal leaf's failure settlement.
    pub(crate) fn reconcile_pending(
        &self,
        destination: &PullDestination,
    ) -> Result<bool, OrbitError> {
        self.reconcile_pending_answer(destination)
            .map(|reconciled| matches!(reconciled, Reconciled::Open))
    }

    /// [`Self::reconcile_pending`], keeping the receipt of an unanswered
    /// request that turned out idle.
    fn reconcile_pending_answer(
        &self,
        destination: &PullDestination,
    ) -> Result<Reconciled, OrbitError> {
        let fenced = FencedPeer {
            inner: self.peer,
            failed: RefCell::new(None),
        };
        let pass = PullDrain {
            jobs: self.jobs,
            peer: &fenced,
            launcher: self.launcher,
            refused_delivery: self.refused_delivery,
        };
        let mut outcome = Reconciled::Open;
        let mut first_error = None;
        // Only admissions holding a slot move: finished history has nothing
        // left to reconcile, and is never read here.
        for record in self.jobs.unsettled_local_pull_admissions()? {
            if record.destination != *destination {
                continue;
            }
            match pass.reconcile(record) {
                Ok(Reconciled::Open) => {}
                // The latest idle answer wins; a held settlement never hides it.
                Ok(idle @ Reconciled::Idle(_)) => outcome = idle,
                Ok(Reconciled::Held) => {
                    if matches!(outcome, Reconciled::Open) {
                        outcome = Reconciled::Held;
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(outcome),
        }
    }

    /// The pass of a gracefully cancelled drain over the admissions it
    /// `carries`: carry each as far as it goes under [`SettleScope::Cancel`]
    /// — deliver what is recorded, settle what ended, release what never
    /// launched — and leave live leaves running. Never requests new work.
    /// Fenced like [`Self::reconcile_pending`], and one stuck admission does
    /// not hold the others back; the first error is returned afterwards.
    pub(crate) fn release_pending(
        &self,
        carries: &dyn Fn(&LocalPullAdmission) -> bool,
        cause: &str,
    ) -> Result<(), OrbitError> {
        let fenced = FencedPeer {
            inner: self.peer,
            failed: RefCell::new(None),
        };
        let pass = PullDrain {
            jobs: self.jobs,
            peer: &fenced,
            launcher: self.launcher,
            refused_delivery: self.refused_delivery,
        };
        let mut first_error = None;
        for mut record in self.jobs.unsettled_local_pull_admissions()? {
            if !carries(&record) {
                continue;
            }
            if let Err(error) =
                pass.carry_settlement(&mut record, SettleScope::Cancel, &|_| true, cause)
            {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Admissions for `destination` that still hold a slot: not idle, refused
    /// or settled. The drain keeps running past its window until this is zero.
    pub(crate) fn unsettled(&self, destination: &PullDestination) -> Result<usize, OrbitError> {
        Ok(self
            .jobs
            .unsettled_local_pull_admissions()?
            .iter()
            .filter(|record| record.destination == *destination)
            .count())
    }

    /// How many of this drain's most recent settled claims failed in a row.
    ///
    /// Reads only admissions `run_id` made, in admission order, so an earlier
    /// drain's history never trips a new one. A claim closed obsolete — the
    /// owner had already ended it — says nothing about this executor, so it
    /// neither extends nor resets the streak.
    pub(crate) fn consecutive_failed_settlements(
        &self,
        destination: &PullDestination,
        run_id: &str,
    ) -> Result<usize, OrbitError> {
        self.jobs
            .consecutive_failed_local_pull_settlements(destination, run_id)
    }

    fn update(
        &self,
        record: &LocalPullAdmission,
        mutation: LocalPullMutation,
    ) -> Result<LocalPullAdmission, OrbitError> {
        self.jobs
            .mutate_local_pull(&record.destination, &record.request.request_id, &mutation)
    }

    /// Send, or re-send, an unanswered request.
    ///
    /// An owner refusal is reconciled against the owner's receipt before the
    /// request is closed: a retry can be refused (say, after the owner was
    /// upgraded) even though an earlier send of the same ID committed a claim,
    /// and that claim must be carried forward, not abandoned. Only when the
    /// owner holds no live receipt is the request closed — and the refusal is
    /// still returned, so this pass allocates nothing further against an owner
    /// that is refusing.
    fn request(&self, record: &LocalPullAdmission) -> Result<LocalPullAdmission, OrbitError> {
        let refusal = match self.peer.request(&record.destination, &record.request) {
            Ok(receipt) => {
                return self.update(record, LocalPullMutation::Receive(Box::new(receipt)));
            }
            Err(error) if is_owner_refusal(&error) => error,
            Err(error) => return Err(error),
        };
        match self
            .peer
            .lookup(&record.destination, &record.request.request_id)?
        {
            AdmissionLookup::Found { receipt, .. } => {
                self.update(record, LocalPullMutation::Receive(receipt))
            }
            AdmissionLookup::Expired | AdmissionLookup::NotFound => {
                self.update(record, LocalPullMutation::Refuse(refusal.to_string()))?;
                Err(refusal)
            }
        }
    }

    /// Returns [`Reconciled::Idle`] for a newly reconciled idle receipt, and
    /// [`Reconciled::Held`] for a settlement the owner refuses while holding
    /// its claim: nothing new is admitted against an owner that will not
    /// accept what this executor already owes it. Historical idle records are
    /// skipped so the next polling pass can allocate a fresh ID.
    fn reconcile(&self, mut record: LocalPullAdmission) -> Result<Reconciled, OrbitError> {
        if matches!(
            record.phase,
            LocalPullPhase::Idle | LocalPullPhase::Settled | LocalPullPhase::Refused
        ) {
            return Ok(Reconciled::Open);
        }
        loop {
            // A queued leaf can be cancelled while the owner bind response is
            // lost. Reconcile that terminal state before retrying bind/launch;
            // stopping the parent alone does not cancel its children.
            if matches!(
                record.phase,
                LocalPullPhase::Created | LocalPullPhase::Bound
            ) && let Some(settling) = self.settle_terminal_leaf(&record)?
            {
                record = settling;
            }
            record = match record.phase {
                LocalPullPhase::Requested => self.request(&record)?,
                LocalPullPhase::Claimed => self.update(&record, LocalPullMutation::CreateLeaf)?,
                LocalPullPhase::Created => match self.try_bind(&record)? {
                    Bind::Bound(bound) => *bound,
                    Bind::Refused(refusal) => return self.close_refused_bind(&record, refusal),
                },
                LocalPullPhase::Bound => {
                    record = self.update(&record, LocalPullMutation::LaunchIntent)?;
                    if let Err(error) = self.launcher.launch(&record) {
                        match error {
                            WorkerLaunchError::NotStarted(error) => {
                                let why = format!("leaf launch failed before spawning: {error}");
                                let mut evidence = release_evidence(&record, &why);
                                evidence.failure = Some(ClaimFailure {
                                    class: ClaimFailureClass::Environment,
                                    reason: why,
                                    crew: record
                                        .receipt
                                        .as_ref()
                                        .and_then(|receipt| receipt.task.as_ref())
                                        .and_then(|task| task.crew.clone()),
                                    candidate: None,
                                });
                                record = self
                                    .record_settlement(&record, ClaimMutation::Release(evidence))?;
                                self.launcher.cancel_queued(&record)?;
                                record = self.reread(&record)?.unwrap_or(record);
                                if record.phase != LocalPullPhase::Settled
                                    && !self.release_held(&record)?
                                {
                                    self.deliver(&record)?;
                                }
                                return Err(error);
                            }
                            // The child may have executed before its failed handoff.
                            // Retain Launching, even if the supervisor stopped it.
                            WorkerLaunchError::Uncertain(error) => return Err(error),
                        }
                    }
                    self.update(&record, LocalPullMutation::Launched)?
                }
                // A terminal leaf ended whatever the launch did, so its failure
                // is recorded; a leaf that may still be live is not restarted.
                LocalPullPhase::Launching => match self.settle_terminal_leaf(&record)? {
                    Some(settling) => settling,
                    None => {
                        return Err(OrbitError::JobValidation("claimed leaf launch is uncertain; deliberate recovery is required, never generic resume".into()));
                    }
                },
                LocalPullPhase::Launched => match self.settle_terminal_leaf(&record)? {
                    Some(settling) => settling,
                    None => return Ok(Reconciled::Open),
                },
                LocalPullPhase::Settling if self.release_held(&record)? => {
                    return Ok(Reconciled::Open);
                }
                LocalPullPhase::Settling => {
                    let delivered = self.deliver(&record)?;
                    if delivered.phase == LocalPullPhase::Settling {
                        return Ok(Reconciled::Held);
                    }
                    delivered
                }
                LocalPullPhase::Settled | LocalPullPhase::Refused => return Ok(Reconciled::Open),
                LocalPullPhase::Idle => {
                    return Ok(match record.receipt {
                        Some(receipt) => Reconciled::Idle(Box::new(receipt)),
                        None => Reconciled::Held,
                    });
                }
            };
        }
    }

    /// Carry one admission's settlement as far as it goes without its drain
    /// [ORB-13663].
    ///
    /// `record` is advanced in place, so after an error it still shows how far
    /// the admission got. Never requests new work or launches a leaf: a
    /// `Launching` record whose leaf is still live stays for deliberate
    /// recovery, a live leaf is left to settle itself, and under
    /// [`SettleScope::Deliver`] an admission that has not launched yet is left
    /// to the drain that owns it.
    ///
    /// Under [`SettleScope::Abandon`], `no_live_drain` is asked again before
    /// each step that would end unlaunched work. Delivery can block for the
    /// routed timeout, so a drain that started since the pass read the live
    /// drains must not have a queued leaf cancelled under it.
    ///
    /// `cause` says why unlaunched work is released; it is the reason the
    /// owner's task carries back to the backlog.
    pub(crate) fn carry_settlement(
        &self,
        record: &mut LocalPullAdmission,
        scope: SettleScope,
        no_live_drain: &dyn Fn(&LocalPullAdmission) -> bool,
        cause: &str,
    ) -> Result<(), OrbitError> {
        let abandon = |record: &LocalPullAdmission| match scope {
            SettleScope::Deliver => false,
            SettleScope::Abandon => no_live_drain(record),
            SettleScope::Cancel => true,
        };
        loop {
            let next = match record.phase {
                LocalPullPhase::Idle | LocalPullPhase::Settled | LocalPullPhase::Refused => {
                    return Ok(());
                }
                LocalPullPhase::Settling if self.release_held(record)? => return Ok(()),
                LocalPullPhase::Settling => {
                    let delivered = self.deliver(record)?;
                    if delivered.phase == LocalPullPhase::Settling {
                        *record = delivered;
                        return Ok(());
                    }
                    delivered
                }
                LocalPullPhase::Launched | LocalPullPhase::Launching => {
                    match self.settle_terminal_leaf(record)? {
                        Some(settling) => settling,
                        None => return Ok(()),
                    }
                }
                LocalPullPhase::Created | LocalPullPhase::Bound => {
                    match self.settle_terminal_leaf(record)? {
                        Some(settling) => settling,
                        None if abandon(record) => {
                            match self.abandon_queued_leaf(record, &abandon, cause)? {
                                Some(settling) => settling,
                                None => return Ok(()),
                            }
                        }
                        None => return Ok(()),
                    }
                }
                LocalPullPhase::Claimed if abandon(record) => self.record_settlement(
                    record,
                    release_settlement(record, &format!("{cause}; no leaf was created")),
                )?,
                // An unanswered request no drain will retry: take the owner's
                // receipt if one was committed, so its claim is released too.
                // With none, nothing is held on the owner; a later drain for
                // this owner re-sends the same ID and carries whatever it
                // finds. The cancelling drain itself withdraws it instead: it
                // is the only sender, and it must be able to finish.
                LocalPullPhase::Requested if abandon(record) => match self
                    .peer
                    .lookup(&record.destination, &record.request.request_id)?
                {
                    AdmissionLookup::Found { receipt, .. } => {
                        self.update(record, LocalPullMutation::Receive(receipt))?
                    }
                    AdmissionLookup::Expired | AdmissionLookup::NotFound
                        if scope == SettleScope::Cancel =>
                    {
                        self.update(
                            record,
                            LocalPullMutation::Refuse(format!(
                                "withdrawn unanswered: {cause}; the owner holds no receipt for it"
                            )),
                        )?
                    }
                    AdmissionLookup::Expired | AdmissionLookup::NotFound => return Ok(()),
                },
                LocalPullPhase::Claimed | LocalPullPhase::Requested => return Ok(()),
            };
            *record = next;
        }
    }

    /// The settlement a terminal leaf implies, recorded; `None` while the leaf
    /// is still live. A leaf that succeeded recorded its typed handoff before
    /// its run terminalized, so a terminal leaf reaching here failed.
    ///
    /// A `Created` admission is bound first: its bind may or may not have
    /// reached the owner, and a failure settlement naming the leaf is fenced
    /// against the owner's binding, so settling it unbound could be refused
    /// forever.
    fn settle_terminal_leaf(
        &self,
        record: &LocalPullAdmission,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        let id = record
            .leaf_run_id
            .as_deref()
            .ok_or_else(|| OrbitError::Store("launched leaf binding missing".into()))?;
        let run = self.jobs.get_job_run(id)?.ok_or_else(|| {
            OrbitError::Store("bound leaf disappeared; deliberate recovery required".into())
        })?;
        if !run.state.is_terminal() {
            return Ok(None);
        }
        let record = self.ensure_bound(record)?;
        let state = self.jobs.read_run_state(id)?;
        let settlement = leaf_failure_settlement(&record, &run, None, state.as_ref());
        self.record_settlement(&record, settlement).map(Some)
    }

    /// Release a queued leaf's claim back to the owner and cancel the leaf,
    /// so it can never start. `record` is advanced to its bound form in
    /// place. `None` when `may_abandon` says, after the bind that may have
    /// blocked, that a live drain now carries the admission: its queued leaf
    /// is left alone.
    ///
    /// The release is recorded before the leaf is cancelled, so the leaf's
    /// terminalization finds the claim's settlement already decided and the
    /// owner's task carries this pass's `cause`.
    fn abandon_queued_leaf(
        &self,
        record: &mut LocalPullAdmission,
        may_abandon: &dyn Fn(&LocalPullAdmission) -> bool,
        cause: &str,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        *record = self.ensure_bound(record)?;
        if !may_abandon(record) {
            return Ok(None);
        }
        let leaf = record.leaf_run_id.as_deref().unwrap_or("-");
        let settling = self.record_settlement(
            record,
            release_settlement(
                record,
                &format!("{cause}; its queued leaf {leaf} never launched"),
            ),
        )?;
        self.launcher.cancel_queued(&settling)?;
        // The cancel may already have delivered the release.
        Ok(Some(self.reread(&settling)?.unwrap_or(settling)))
    }

    /// Bind a `Created` admission's leaf on the owner. Binding is idempotent:
    /// a replay returns the recorded outcome and never substitutes a run. An
    /// owner refusal is reported as [`Bind::Refused`]; a lost or uncertain
    /// delivery is returned.
    fn try_bind(&self, record: &LocalPullAdmission) -> Result<Bind, OrbitError> {
        match self.peer.bind(record) {
            Ok(()) => self
                .update(record, LocalPullMutation::Bound)
                .map(|bound| Bind::Bound(Box::new(bound))),
            Err(error) if is_owner_refusal(&error) => Ok(Bind::Refused(error)),
            Err(error) => Err(error),
        }
    }

    /// [`Self::try_bind`] for a settle-only pass: a `Created` admission the
    /// owner refuses to bind is returned as it was and left to settlement,
    /// which reconciles the refusal against the owner's receipt.
    fn ensure_bound(&self, record: &LocalPullAdmission) -> Result<LocalPullAdmission, OrbitError> {
        if record.phase != LocalPullPhase::Created {
            return Ok(record.clone());
        }
        match self.try_bind(record)? {
            Bind::Bound(bound) => Ok(*bound),
            Bind::Refused(_) => Ok(record.clone()),
        }
    }

    /// End a `Created` admission whose bind the owner refused — an operator
    /// revoked or recovered the claim while the bind response was lost, say.
    ///
    /// Retrying the bind would be refused on every pass, leaving the record
    /// `Created` and holding its slot forever, so its failure is recorded and
    /// delivered like any other. Delivery reconciles against the owner's
    /// receipt: a claim the owner has already ended closes the record
    /// obsolete, and settling it fails the never-launched leaf so nothing can
    /// start it. A claim the owner still holds keeps the settlement pending
    /// (an operator revokes or recovers it on the owner to release the slot).
    /// The refusal is returned either way, so this pass admits nothing
    /// further against an owner that is refusing.
    fn close_refused_bind(
        &self,
        record: &LocalPullAdmission,
        refusal: OrbitError,
    ) -> Result<Reconciled, OrbitError> {
        tracing::warn!(
            target: "orbit.core.pull",
            request_id = %record.request.request_id,
            leaf = record.leaf_run_id.as_deref().unwrap_or("-"),
            %refusal,
            "the owner refused to bind a claimed leaf; ending the claim without launching it",
        );
        let settlement = ClaimMutation::Fail(ClaimEvidence {
            summary: Some(format!(
                "Outcome: failed\nowner refused bind: {refusal}; the leaf was never launched, \
                 so nothing ran."
            )),
            ..Default::default()
        });
        let settling = self.record_settlement(record, settlement)?;
        self.deliver(&settling)?;
        Err(refusal)
    }

    /// Record `settlement` as this admission's pending settlement.
    ///
    /// Several follower processes may settle the same admission — the leaf's
    /// own worker as it terminalizes, a drain pass, a cancel — and a recorded
    /// settlement is immutable. When the write loses to one already recorded,
    /// that one is carried forward: the first recorded settlement is the one
    /// the owner receives.
    pub(crate) fn record_settlement(
        &self,
        record: &LocalPullAdmission,
        settlement: ClaimMutation,
    ) -> Result<LocalPullAdmission, OrbitError> {
        match self.update(record, LocalPullMutation::Settle(Box::new(settlement))) {
            Ok(settling) => Ok(settling),
            Err(error) => match self.reread(record)? {
                Some(current) if current.settlement.is_some() => Ok(current),
                _ => Err(error),
            },
        }
    }

    fn reread(
        &self,
        record: &LocalPullAdmission,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        if let Some(leaf) = record.leaf_run_id.as_deref() {
            return self.jobs.local_pull_for_run(leaf);
        }
        Ok(self
            .jobs
            .local_pull_admissions()?
            .into_iter()
            .find(|current| {
                current.destination == record.destination
                    && current.request.request_id == record.request.request_id
            }))
    }

    /// Whether `record`'s settlement is a release that must wait: a forced
    /// cancel recorded it for a leaf it then could not confirm stopped, and
    /// the leaf still runs. Handing its task back to the backlog now could
    /// let a second executor start it beside the first, so the owner keeps
    /// the claim until the leaf is seen to stop. A queued leaf is cancelled
    /// before delivery, including when a prior cancellation attempt failed.
    pub(crate) fn release_held(&self, record: &LocalPullAdmission) -> Result<bool, OrbitError> {
        if !matches!(record.settlement, Some(ClaimMutation::Release(_))) {
            return Ok(false);
        }
        let Some(leaf) = record.leaf_run_id.as_deref() else {
            return Ok(false);
        };
        if self
            .jobs
            .get_job_run(leaf)?
            .is_some_and(|run| run.state == JobRunState::Pending)
        {
            self.launcher.cancel_queued(record)?;
        }
        Ok(self
            .jobs
            .get_job_run(leaf)?
            .is_some_and(|run| !run.state.is_terminal()))
    }

    /// Deliver a persisted settlement to the owner.
    ///
    /// An owner refusal is reconciled against the owner's receipt, the way a
    /// refused request is: when the owner has already ended the claim — an
    /// operator revoked it, or it failed or landed — no settlement can ever be
    /// accepted for it, so the record settles locally with the refusal and
    /// releases its slot. Retrying it would refuse forever, and every pass
    /// would report that error instead of admitting new work. A lost or
    /// uncertain delivery keeps its settlement pending and is returned.
    ///
    /// A claim the owner still holds keeps its settlement pending too, and
    /// the refusal is recorded on it [ORB-13979]: the owner answered, and
    /// will answer the same until an operator changes it (for example, a
    /// footprint widening onto a path the owner protects).
    /// Under [`RefusedDelivery::WhenDue`] the record is then not sent again
    /// until its backoff has elapsed. Either way the record is returned
    /// still `Settling`, and callers stop there.
    pub(crate) fn deliver(
        &self,
        record: &LocalPullAdmission,
    ) -> Result<LocalPullAdmission, OrbitError> {
        if self.refused_delivery == RefusedDelivery::WhenDue
            && record
                .settlement_refusal
                .as_ref()
                .is_some_and(|refusal| chrono::Utc::now() < refusal.retry_after)
        {
            return Ok(record.clone());
        }
        let refusal = match self.peer.settle(record) {
            Ok(()) => return self.update(record, LocalPullMutation::Settled),
            Err(error) if is_owner_refusal(&error) => error,
            Err(error) => return Err(error),
        };
        let ended = match self
            .peer
            .lookup(&record.destination, &record.request.request_id)?
        {
            AdmissionLookup::Found {
                current_claim: Some(claim),
                ..
            } if claim.phase.is_unsettled() => {
                return self.defer_refused_settlement(record, &refusal);
            }
            AdmissionLookup::Found {
                current_claim: Some(claim),
                ..
            } => format!("the owner already ended this claim as {:?}", claim.phase),
            AdmissionLookup::Found {
                current_claim: None,
                ..
            } => "the owner no longer holds this claim".to_string(),
            AdmissionLookup::Expired | AdmissionLookup::NotFound => {
                "the owner no longer holds this admission".to_string()
            }
        };
        let reason = format!("settlement refused ({refusal}); {ended}");
        tracing::warn!(
            target: "orbit.core.pull",
            request_id = %record.request.request_id,
            leaf = record.leaf_run_id.as_deref().unwrap_or("-"),
            %reason,
            "closing an undeliverable pull settlement",
        );
        self.update(record, LocalPullMutation::SettleObsolete(reason))
    }

    /// Record the owner's refusal of `record`'s settlement while it holds the
    /// claim, and when automatic passes next deliver it: each consecutive
    /// refusal doubles the wait, up to a cap. Logged as a warning when the
    /// refusal is new or its reason changed, not on every repeat.
    fn defer_refused_settlement(
        &self,
        record: &LocalPullAdmission,
        refusal: &OrbitError,
    ) -> Result<LocalPullAdmission, OrbitError> {
        let now = chrono::Utc::now();
        let reason = refusal.to_string();
        let previous = record.settlement_refusal.as_ref();
        let refusals = previous
            .map_or(0, |previous| previous.refusals)
            .saturating_add(1);
        let backoff = settlement_refusal_backoff(refusals);
        let retry_after =
            now + chrono::Duration::from_std(backoff).unwrap_or_else(|_| chrono::Duration::zero());
        if previous.is_none_or(|previous| previous.reason != reason) {
            tracing::warn!(
                target: "orbit.core.pull",
                owner = %record.destination.selector,
                request_id = %record.request.request_id,
                leaf = record.leaf_run_id.as_deref().unwrap_or("-"),
                %reason,
                retry_after = %retry_after.to_rfc3339(),
                "the owner refused a pending settlement while holding its claim; it stays \
                 recorded, and delivery backs off until the owner accepts it",
            );
        } else {
            tracing::debug!(
                target: "orbit.core.pull",
                request_id = %record.request.request_id,
                refusals,
                retry_after = %retry_after.to_rfc3339(),
                "pending settlement refused again",
            );
        }
        self.update(
            record,
            LocalPullMutation::DeferSettlement(SettlementRefusal {
                reason,
                refusals,
                first_refused_at: previous.map_or(now, |previous| previous.first_refused_at),
                last_refused_at: now,
                retry_after,
            }),
        )
    }
}

/// Largest failure excerpt a settlement carries. The whole diagnostic stays in
/// the executor's run record; the owner's reader needs enough to decide.
const MAX_FAILURE_EXCERPT_BYTES: usize = 8 * 1024;

/// The settlement a terminal leaf implies, by how far its admission got: a
/// leaf that never launched was cancelled while queued, so nothing ran and
/// its claim is released back to the owner's backlog. A launched leaf ended
/// without the typed handoff success records, and its settlement carries why
/// as a typed [`ClaimFailure`] [ORB-14257]: a class that
/// [blocks](ClaimFailureClass::blocks) — the candidate's, or the task's own —
/// fails the claim, as does an operator's cancel that asked to block the task
/// [ORB-14274]; any other (an operator's cancel, a provider this host
/// could not use [ORB-13941], a missing validation tool, an unreachable
/// owner, a red base, an inconclusive or interrupted run) releases it, and
/// the drain excludes the leaf's crew for its window when the class
/// [says so](ClaimFailureClass::excludes_crew).
///
/// Every follower process that settles a terminal leaf computes it here, so
/// the leaf's own worker, a cancel and a drain pass agree on the value.
/// `diagnostic` is the `(code, message)` the terminalizing caller knows before
/// its diagnostic step is durable; `state` is the leaf's pipeline state.
///
/// [ORB-13907] The leaf's recorded final recovery decision rides on a failure
/// settlement for the owner to apply — a follower never writes its owner's
/// task — unless it was `resume`, whose rerun then failed on its own.
///
/// A `held` leaf whose review settled into an evidence hold did not fail; it
/// is released with the typed hold ([`evidence_hold_release`]).
pub(crate) fn leaf_failure_settlement(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    state: Option<&PipelineState>,
) -> ClaimMutation {
    if matches!(
        record.phase,
        LocalPullPhase::Created | LocalPullPhase::Bound
    ) {
        return release_settlement(
            record,
            &format!(
                "its queued leaf {} terminated as {} before launch",
                run.run_id, run.state
            ),
        );
    }
    if run.state == JobRunState::Held
        && let Some(hold) = held_evidence(state)
    {
        return evidence_hold_release(record, run, hold);
    }
    let final_recovery = state
        .and_then(|state| state.final_recovery.as_ref())
        .and_then(|checkpoint| checkpoint.decision.clone())
        .filter(|decision| !matches!(decision, FinalRecoveryDecision::Resume { .. }));
    let failure = leaf_failure(record, run, diagnostic, final_recovery.as_ref(), state);
    // [ORB-14274] An operator who cancelled with `--block` asked for the
    // legacy outcome: the cancel stays typed but fails the claim.
    let operator_blocks = run.state == JobRunState::Cancelled
        && state
            .and_then(|state| state.task_cancellation_policy.as_ref())
            .is_some_and(|policy| policy.block);
    if !failure.class.blocks() && !operator_blocks {
        // [ORB-14258] A required command the base fails exactly as the
        // candidate does releases with the typed hold, which the owner
        // records so its admission waits for the base to move.
        let hold = (failure.class == ClaimFailureClass::BaselineRed)
            .then(|| baseline_red(run, diagnostic))
            .flatten();
        let mut evidence = match &hold {
            Some(hold) => baseline_red_release(record, run, hold, &failure),
            None => release_evidence(record, &release_reason(run, &failure)),
        };
        evidence.baseline_red = hold;
        if failure.class == ClaimFailureClass::Provider {
            evidence.provider_unavailable = Some(ProviderUnavailable {
                crew: failure.crew.clone(),
                reason: failure.reason.clone(),
            });
        }
        evidence.failure = Some(failure);
        return ClaimMutation::Release(evidence);
    }
    let mut summary = terminal_failure_summary_with(run, diagnostic);
    let final_recovery = final_recovery.map(|decision| {
        summary.push_str(&format!(
            "\nFinal recovery decided `{}`; the owner applies it.",
            decision.kind()
        ));
        ClaimFinalRecovery {
            run_id: run.run_id.clone(),
            decision,
        }
    });
    ClaimMutation::Fail(ClaimEvidence {
        summary: Some(summary),
        final_recovery,
        failure: Some(failure),
        ..Default::default()
    })
}

/// The evidence hold a held leaf's review settlement recorded in its run
/// state, under the `review_gate_settle` step's output.
fn held_evidence(state: Option<&PipelineState>) -> Option<ReviewEvidenceHold> {
    let hold = state?
        .pipeline
        .get("review_gate_settle")?
        .get("evidence_hold")?;
    serde_json::from_value(hold.clone()).ok()
}

/// A `held` leaf whose review settled into an evidence hold did not fail: its
/// claim is released with the typed hold, which the owner records as the
/// task's latest decision.
fn evidence_hold_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: ReviewEvidenceHold,
) -> ClaimMutation {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let names = hold
        .requirements
        .iter()
        .map(|requirement| format!("`{}`", requirement.name))
        .collect::<Vec<_>>()
        .join(", ");
    let why = format!(
        "leaf {} held its reviewed candidate {} for named external evidence ({names})",
        run.run_id, hold.candidate.commit
    );
    ClaimMutation::Release(ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task stays in \
             progress under the evidence hold; attaching every named result queues a fresh \
             review. No pull request was opened."
        )),
        evidence_hold: Some(hold),
        ..Default::default()
    })
}

/// Largest reason a typed failure carries.
const MAX_FAILURE_REASON_BYTES: usize = 1024;

/// Why a launched leaf ended, typed. An operator's cancel wins; then the
/// typed marker of the last failed step, of any provider or red-base failure
/// the run recorded, or of the terminalizing caller's diagnostic; then a
/// worker that died; then a final recovery that judged the task itself; then
/// a committed candidate that could not be synchronized onto its base.
/// Anything else is the candidate's.
fn leaf_failure(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
    final_recovery: Option<&FinalRecoveryDecision>,
    state: Option<&PipelineState>,
) -> ClaimFailure {
    let last_failed = run
        .steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some());
    let step_class = |step: &orbit_types::workflow::JobRunStep| {
        ClaimFailureClass::of_step_failure(
            step.error_code.as_deref(),
            step.error_message.as_deref(),
        )
        .map(|class| (class, step.error_message.clone().unwrap_or_default()))
    };
    let typed = if run.state == JobRunState::Cancelled {
        // The operator's recorded cancel note names who stopped it and why.
        let reason = state
            .and_then(|state| state.task_cancellation_policy.as_ref())
            .map(|policy| policy.note.clone())
            .or_else(|| diagnostic.map(|(_, message)| message.to_string()))
            .unwrap_or_else(|| format!("leaf {} was cancelled", run.run_id));
        Some((ClaimFailureClass::OperatorCancel, reason))
    } else {
        last_failed
            .and_then(step_class)
            .or_else(|| {
                run.steps.iter().rev().find_map(|step| {
                    step_class(step).filter(|(class, _)| {
                        matches!(
                            class,
                            ClaimFailureClass::Provider | ClaimFailureClass::BaselineRed
                        )
                    })
                })
            })
            .or_else(|| {
                diagnostic.and_then(|(code, message)| {
                    ClaimFailureClass::of_step_failure(Some(code), Some(message))
                        .map(|class| (class, message.to_string()))
                })
            })
    };
    let stopped_at = state.and_then(|state| first_incomplete_step(run, state));
    let (class, reason) = typed.unwrap_or_else(|| {
        let reason = last_failed
            .and_then(|step| step.error_message.clone())
            .or_else(|| diagnostic.map(|(_, message)| message.to_string()))
            .unwrap_or_else(|| format!("leaf {} terminated as {}", run.run_id, run.state));
        let class = if run.state == JobRunState::Interrupted {
            ClaimFailureClass::Transient
        } else if matches!(
            final_recovery,
            Some(FinalRecoveryDecision::Reject { .. } | FinalRecoveryDecision::Archive { .. })
        ) {
            ClaimFailureClass::TaskInput
        } else if stopped_at == Some(SYNC_BASE_STEP) {
            // The committed candidate is intact; only the base moved under
            // it, and the step's conflict recovery could not carry it over.
            ClaimFailureClass::BaseConflict
        } else {
            ClaimFailureClass::Candidate
        };
        (class, reason)
    });
    // [ORB-14617] A forge hold's text leads with its JSON; say it plainly.
    let mut reason = match ForgeUnavailableHold::from_text(&reason) {
        Some(hold) => format!(
            "the forge refused the push of {} to {} {} times over {} s",
            hold.head_sha,
            hold.target_ref,
            hold.attempts,
            hold.waited_ms / 1000
        ),
        None => reason,
    };
    for marker in FAILURE_MARKERS {
        reason = reason.replace(marker, "");
    }
    let reason = reason.trim();
    let cut = floor_char_boundary(reason, MAX_FAILURE_REASON_BYTES);
    let crew = run
        .resolved_crew
        .clone()
        .filter(|crew| !crew.trim().is_empty())
        .or_else(|| {
            record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.task.as_ref())
                .and_then(|task| task.crew.clone())
        });
    ClaimFailure {
        class,
        reason: reason[..cut].to_string(),
        crew,
        candidate: state.and_then(|state| preserved_candidate(run, state)),
    }
}

/// Orbit's typed failure markers, which a failure's reason quotes without.
const FAILURE_MARKERS: [&str; 7] = [
    FORGE_UNAVAILABLE_MARKER,
    PROVIDER_UNAVAILABLE_MARKER,
    PROVIDER_CAPACITY_MARKER,
    VALIDATION_ENVIRONMENT_MARKER,
    OWNER_ROUTE_UNAVAILABLE_MARKER,
    BASELINE_RED_MARKER,
    TRANSIENT_FAILURE_MARKER,
];

/// The baseline hold a terminal leaf failed on: a failed step, or the
/// terminalizing caller's own diagnostic, carrying the typed
/// `[baseline_red]` failure `claim_validate` raises.
fn baseline_red(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
) -> Option<BaselineRedHold> {
    run.steps
        .iter()
        .rev()
        .find(|step| {
            is_baseline_red_failure(step.error_code.as_deref(), step.error_message.as_deref())
        })
        .and_then(|step| step.error_message.as_deref())
        .and_then(BaselineRedHold::from_text)
        .or_else(|| {
            diagnostic
                .filter(|(code, message)| is_baseline_red_failure(Some(code), Some(message)))
                .and_then(|(_, message)| BaselineRedHold::from_text(message))
        })
        .map(|mut hold| {
            if hold.run_id.is_empty() {
                hold.run_id.clone_from(&run.run_id);
            }
            hold
        })
}

/// [ORB-14258] The release of a leaf whose required command fails on its
/// base exactly as on its candidate: the owner holds the task until the base
/// moves to a commit where the command passes.
fn baseline_red_release(
    record: &LocalPullAdmission,
    run: &orbit_types::workflow::JobRun,
    hold: &BaselineRedHold,
    failure: &ClaimFailure,
) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    let why = format!(
        "leaf {} ended on a `{}` failure: required validation `{}` is red on base {} exactly \
         as on its candidate{}",
        run.run_id,
        failure.class.as_str(),
        hold.command,
        hold.base_sha,
        candidate_note(failure)
    );
    let base_ref = if hold.base_ref.is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref)
    };
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task is back \
             in the backlog, held until {base_ref} moves to a base where the command passes; no \
             pull request was opened."
        )),
        ..Default::default()
    }
}

/// The release comment's reason for a typed failure.
fn release_reason(run: &orbit_types::workflow::JobRun, failure: &ClaimFailure) -> String {
    let crew = failure.crew.as_deref().unwrap_or("its crew");
    let mut why = match failure.class {
        ClaimFailureClass::Provider => format!(
            "leaf {} ended on a `provider` failure: it could not use the provider of crew \
             `{crew}` on this host ({}); the work was not attempted",
            run.run_id, failure.reason
        ),
        ClaimFailureClass::OperatorCancel => {
            format!("its leaf {} was cancelled ({})", run.run_id, failure.reason)
        }
        ClaimFailureClass::BaseConflict => format!(
            "leaf {} ended on a `base_conflict` failure: its committed candidate could not be \
             synchronized onto a base that moved under it ({})",
            run.run_id, failure.reason
        ),
        class => format!(
            "leaf {} ended on a `{}` failure that is not the candidate's ({})",
            run.run_id,
            class.as_str(),
            failure.reason
        ),
    };
    if failure.class.suppresses_host() {
        why.push_str(", and this drain claims no more work on this host in its window");
    } else if failure.class.excludes_crew() {
        why.push_str(&format!(
            ", and this drain runs no more `{crew}` tasks in its window"
        ));
    }
    why.push_str(&candidate_note(failure));
    why
}

/// Hand an unfinished claim back to the owner: the task returns to the
/// backlog, and the comment names the drain that held it and why it gave it
/// back. Nothing is recorded as a failure — the work either never ran or was
/// stopped on purpose.
pub(crate) fn release_settlement(record: &LocalPullAdmission, why: &str) -> ClaimMutation {
    ClaimMutation::Release(release_evidence(record, why))
}

/// [`release_settlement`] for a launched leaf an operator stopped: typed
/// [`ClaimFailureClass::OperatorCancel`], so the owner returns the task to
/// backlog with the cancel reason and counts the release against its budget.
pub(crate) fn operator_cancel_release(record: &LocalPullAdmission, why: &str) -> ClaimMutation {
    let mut evidence = release_evidence(record, why);
    evidence.failure = Some(ClaimFailure {
        class: ClaimFailureClass::OperatorCancel,
        reason: why.to_string(),
        crew: record
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.task.as_ref())
            .and_then(|task| task.crew.clone()),
        candidate: None,
    });
    ClaimMutation::Release(evidence)
}

fn release_evidence(record: &LocalPullAdmission, why: &str) -> ClaimEvidence {
    let drain = &record.request.run_context.run_id;
    let machine = &record.destination.execution_machine_id;
    ClaimEvidence {
        summary: Some(format!("released by follower drain {drain}: {why}")),
        comment: Some(format!(
            "Follower drain {drain} on {machine} released this claim: {why}. The task is back \
             in the backlog and can be pulled again."
        )),
        ..Default::default()
    }
}

fn terminal_failure_summary_with(
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
) -> String {
    let mut summary = format!(
        "Outcome: failed\nClaimed leaf {} terminated as {} without an acknowledged typed handoff.",
        run.run_id, run.state
    );
    let failed_step = run
        .steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some());
    if let Some(step) = failed_step {
        summary.push_str(&format!("\nFailed step: {}", step.target_id));
        if let Some(code) = step.error_code.as_deref() {
            summary.push_str(&format!(" ({code})"));
        }
        if let Some(message) = step
            .error_message
            .as_deref()
            .map(str::trim)
            .filter(|message| !message.is_empty())
        {
            let cut = floor_char_boundary(message, MAX_FAILURE_EXCERPT_BYTES);
            summary.push_str("\nError: ");
            summary.push_str(&message[..cut]);
            if cut < message.len() {
                summary.push_str(&format!(" [truncated to {cut} of {} bytes]", message.len()));
            }
        }
    } else if let Some((code, message)) = diagnostic {
        summary.push_str(&format!(
            "\nTerminal diagnostic ({code}): {}",
            message.trim()
        ));
    }
    summary.push_str(&format!(
        "\nInspect the run on its execution host: `orbit run show {}`.",
        run.run_id
    ));
    summary
}
