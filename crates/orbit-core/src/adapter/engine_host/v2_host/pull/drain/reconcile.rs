//! Pending-admission reconciliation: carry each earlier admission forward
//! through request, bind and launch, behind a fenced owner peer.
use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimFailure, ClaimMutation,
    LocalPullAdmission, LocalPullMutation, LocalPullPhase, PullDestination,
};
use orbit_types::workflow::ClaimFailureClass;

use super::failure::release_evidence;
use super::{Bind, PullDrain, PullPeer, Reconciled, SettleScope};
use crate::application::distributed::{is_owner_refusal, is_owner_transport_failure};
use crate::application::job::pipeline::WorkerLaunchError;

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

impl PullDrain<'_> {
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
    pub(super) fn reconcile_pending_answer(
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

    pub(super) fn update(
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
    /// owner holds no live receipt is the request closed. A refusal of an
    /// obsolete persisted request is then reconciled successfully: it says
    /// nothing about this build's compatibility with the owner. That covers
    /// protocol skew of a fingerprint this build would not send, and an owner
    /// refusal of an older revision, which an owner answers as `invalid_input`
    /// for a fingerprint-less caller. Other refusals are returned, so this pass
    /// allocates nothing further against that owner.
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
                let closed = self.update(record, LocalPullMutation::Refuse(refusal.to_string()))?;
                let obsolete = record.request.caller_fingerprint.as_deref()
                    != Some(orbit_store::contracts::distributed_drain_protocol_fingerprint());
                let older_revision = record.request.caller_schema
                    != orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA;
                if obsolete
                    && (matches!(refusal, OrbitError::ProtocolSkew(_))
                        || (older_revision && is_owner_refusal(&refusal)))
                {
                    Ok(closed)
                } else {
                    Err(refusal)
                }
            }
        }
    }

    /// Returns [`Reconciled::Idle`] for a newly reconciled idle receipt, and
    /// [`Reconciled::Held`] for a settlement the owner refuses while holding
    /// its claim: nothing new is admitted against an owner that will not
    /// accept what this executor already owes it. Historical idle records are
    /// skipped so the next polling pass can allocate a fresh ID.
    pub(super) fn reconcile(
        &self,
        mut record: LocalPullAdmission,
    ) -> Result<Reconciled, OrbitError> {
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
                                    provider_limit: false,
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
}
