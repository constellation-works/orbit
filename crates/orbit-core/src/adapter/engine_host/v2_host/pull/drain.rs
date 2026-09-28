//! Pull refill loop. The owner transport and the leaf launcher are injected;
//! the loop owns only the durable checkpoint discipline between them.
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionLookup, AdmissionReceipt, AdmissionRequest, ClaimEvidence, ClaimMutation,
    JobRunStoreBackend, LocalPullAdmission, LocalPullMutation, LocalPullPhase, PullDestination,
};

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
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    /// Cancel a bound leaf that was never launched, so it can never start.
    /// Only a settle-only pass abandoning an admission no live drain will
    /// carry calls this ([`SettleScope::Abandon`]); the leaf's pending run has
    /// no process.
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
    /// and none pulls from its owner: end what was never launched, cancelling
    /// a queued leaf, and settle the claim as a failure, so the owner is never
    /// left holding a claim no follower process is responsible for. Live
    /// leaves are left running; they settle themselves when they terminalize.
    Abandon,
}

/// Whether the owner answered a pull with a refusal, as opposed to a lost or
/// uncertain delivery.
///
/// Only an answer counts. A refusal from the owner's pre-admission ladder —
/// selector, capability, shape, version, ship mode, review policy, a stale
/// ship contract — commits nothing, so it is safe to close the request once
/// the owner also confirms it holds no receipt for it. A delivery miss, a lost
/// answer or a store failure says nothing about whether an earlier send of the
/// same request committed, so the request stays pending and is retried.
pub(crate) fn is_owner_refusal(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { code, .. } => matches!(
            code.as_str(),
            "invalid_input" | "capability_refused" | "capability_denied" | "policy_denied"
        ),
        OrbitError::InvalidInput(_)
        | OrbitError::CapabilityRefused(_)
        | OrbitError::CapabilityDenied(_)
        | OrbitError::PolicyDenied(_) => true,
        _ => false,
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

pub(crate) struct PullDrain<'a> {
    pub(crate) jobs: &'a dyn JobRunStoreBackend,
    pub(crate) peer: &'a dyn PullPeer,
    pub(crate) launcher: &'a dyn PullLauncher,
}

impl PullDrain<'_> {
    /// One bounded refill. Each new ID is made durable before the request goes
    /// on the wire. An idle response ends the whole pass, regardless of free
    /// slots. Reconciliation errors prevent all fresh admission.
    ///
    /// The failure breaker is checked after reconciliation, because settling
    /// a leaf that has just failed can be what opens it: that pass must not
    /// request replacements.
    pub(crate) fn refill(
        &self,
        destination: &PullDestination,
        template: &AdmissionRequest,
        ceiling: usize,
    ) -> Result<usize, OrbitError> {
        if !self.reconcile_pending(destination)? {
            return Ok(0);
        }
        if self.consecutive_failed_settlements(destination, &template.run_context.run_id)?
            >= CONSECUTIVE_FAILURE_BREAKER
        {
            return Ok(0);
        }
        let mut admitted = 0;
        for _ in 0..ceiling {
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
            if !self.reconcile(record)? {
                break;
            }
            admitted += 1;
        }
        Ok(admitted)
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
    pub(crate) fn reconcile_pending(
        &self,
        destination: &PullDestination,
    ) -> Result<bool, OrbitError> {
        let mut may_allocate = true;
        let mut first_error = None;
        for record in self.jobs.local_pull_admissions()? {
            if record.destination != *destination {
                continue;
            }
            match self.reconcile(record) {
                Ok(allocate) => may_allocate &= allocate,
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(may_allocate),
        }
    }

    /// Admissions for `destination` that still hold a slot: not idle, refused
    /// or settled. The drain keeps running past its window until this is zero.
    pub(crate) fn unsettled(&self, destination: &PullDestination) -> Result<usize, OrbitError> {
        Ok(self
            .jobs
            .local_pull_admissions()?
            .iter()
            .filter(|record| record.destination == *destination && record.holds_capacity())
            .count())
    }

    /// How many of this drain's most recent settled claims failed in a row.
    ///
    /// Reads only admissions `run_id` made, in admission order, so an earlier
    /// drain's history never trips a new one.
    pub(crate) fn consecutive_failed_settlements(
        &self,
        destination: &PullDestination,
        run_id: &str,
    ) -> Result<usize, OrbitError> {
        Ok(self
            .jobs
            .local_pull_admissions()?
            .iter()
            .filter(|record| {
                record.destination == *destination
                    && record.request.run_context.run_id == run_id
                    && record.phase == LocalPullPhase::Settled
            })
            .rev()
            .take_while(|record| matches!(record.settlement, Some(ClaimMutation::Fail(_))))
            .count())
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

    /// Returns false only for a newly reconciled idle receipt. Historical idle
    /// records are skipped so the next polling pass can allocate a fresh ID.
    fn reconcile(&self, mut record: LocalPullAdmission) -> Result<bool, OrbitError> {
        if matches!(
            record.phase,
            LocalPullPhase::Idle | LocalPullPhase::Settled | LocalPullPhase::Refused
        ) {
            return Ok(true);
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
                LocalPullPhase::Created => {
                    self.peer.bind(&record)?;
                    self.update(&record, LocalPullMutation::Bound)?
                }
                LocalPullPhase::Bound => {
                    record = self.update(&record, LocalPullMutation::LaunchIntent)?;
                    if let Err(error) = self.launcher.launch(&record) {
                        let settlement = ClaimMutation::Fail(ClaimEvidence {
                            summary: Some(format!("leaf launch failed: {error}")),
                            ..Default::default()
                        });
                        record =
                            self.update(&record, LocalPullMutation::Settle(Box::new(settlement)))?;
                        self.deliver(&record)?;
                        return Err(error);
                    }
                    self.update(&record, LocalPullMutation::Launched)?
                }
                LocalPullPhase::Launching => {
                    return Err(OrbitError::JobValidation("claimed leaf launch is uncertain; deliberate recovery is required, never generic resume".into()));
                }
                LocalPullPhase::Launched => match self.settle_terminal_leaf(&record)? {
                    Some(settling) => settling,
                    None => return Ok(true),
                },
                LocalPullPhase::Settling => self.deliver(&record)?,
                LocalPullPhase::Settled | LocalPullPhase::Refused => return Ok(true),
                LocalPullPhase::Idle => return Ok(false),
            };
        }
    }

    /// Carry one admission's settlement as far as it goes without its drain
    /// [ORB-13663].
    ///
    /// `record` is advanced in place, so after an error it still shows how far
    /// the admission got. Never requests new work or launches a leaf: a
    /// `Launching` record stays for deliberate recovery, a live leaf is left
    /// to settle itself, and under [`SettleScope::Deliver`] an admission that
    /// has not launched yet is left to the drain that owns it.
    pub(crate) fn carry_settlement(
        &self,
        record: &mut LocalPullAdmission,
        scope: SettleScope,
    ) -> Result<(), OrbitError> {
        let abandon = scope == SettleScope::Abandon;
        loop {
            let next = match record.phase {
                LocalPullPhase::Idle
                | LocalPullPhase::Settled
                | LocalPullPhase::Refused
                | LocalPullPhase::Launching => return Ok(()),
                LocalPullPhase::Settling => self.deliver(record)?,
                LocalPullPhase::Launched => match self.settle_terminal_leaf(record)? {
                    Some(settling) => settling,
                    None => return Ok(()),
                },
                LocalPullPhase::Created | LocalPullPhase::Bound => {
                    match self.settle_terminal_leaf(record)? {
                        Some(settling) => settling,
                        None if abandon => self.abandon_queued_leaf(record)?,
                        None => return Ok(()),
                    }
                }
                LocalPullPhase::Claimed if abandon => self.record_settlement(
                    record,
                    ClaimMutation::Fail(ClaimEvidence {
                        summary: Some(format!(
                            "Outcome: failed\nThe follower drain {} that claimed this task \
                             ended before it created a leaf for it; nothing ran. Move the task \
                             back to the backlog to run it again.",
                            record.request.run_context.run_id
                        )),
                        ..Default::default()
                    }),
                )?,
                // An unanswered request no drain will retry: take the owner's
                // receipt if one was committed, so its claim is ended too. With
                // none, nothing is held on the owner; a later drain for this
                // owner re-sends the same ID and carries whatever it finds.
                LocalPullPhase::Requested if abandon => match self
                    .peer
                    .lookup(&record.destination, &record.request.request_id)?
                {
                    AdmissionLookup::Found { receipt, .. } => {
                        self.update(record, LocalPullMutation::Receive(receipt))?
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
        let settlement = leaf_failure_settlement(record.phase, &run, None);
        self.record_settlement(&record, settlement).map(Some)
    }

    /// End a cancelled drain's queued leaf and settle its claim.
    fn abandon_queued_leaf(
        &self,
        record: &LocalPullAdmission,
    ) -> Result<LocalPullAdmission, OrbitError> {
        let record = self.ensure_bound(record)?;
        self.launcher.cancel_queued(&record)?;
        self.settle_terminal_leaf(&record)?.ok_or_else(|| {
            OrbitError::JobValidation(
                "the queued leaf of a cancelled drain did not terminalize when cancelled".into(),
            )
        })
    }

    /// Bind a `Created` admission's leaf on the owner. Binding is idempotent:
    /// a replay returns the recorded outcome and never substitutes a run. An
    /// owner refusal is left to settlement, which reconciles it against the
    /// owner's receipt; a lost or uncertain delivery is returned.
    fn ensure_bound(&self, record: &LocalPullAdmission) -> Result<LocalPullAdmission, OrbitError> {
        if record.phase != LocalPullPhase::Created {
            return Ok(record.clone());
        }
        match self.peer.bind(record) {
            Ok(()) => self.update(record, LocalPullMutation::Bound),
            Err(error) if is_owner_refusal(&error) => Ok(record.clone()),
            Err(error) => Err(error),
        }
    }

    /// Record `settlement` as this admission's pending settlement.
    ///
    /// Several follower processes may settle the same admission — the leaf's
    /// own worker as it terminalizes, a drain pass, a cancel — and a recorded
    /// settlement is immutable. When the write loses to one already recorded,
    /// that one is carried forward: the first recorded settlement is the one
    /// the owner receives.
    fn record_settlement(
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

    /// Persist a leaf's settlement, then deliver it. A disconnect leaves
    /// exactly this immutable settlement for a later pass to retry
    /// idempotently.
    #[allow(dead_code)]
    pub(crate) fn settle(
        &self,
        record: &LocalPullAdmission,
        settlement: ClaimMutation,
    ) -> Result<(), OrbitError> {
        let pending = self.update(record, LocalPullMutation::Settle(Box::new(settlement)))?;
        self.deliver(&pending)?;
        Ok(())
    }

    /// Deliver a persisted settlement to the owner.
    ///
    /// An owner refusal is reconciled against the owner's receipt, the way a
    /// refused request is: when the owner has already ended the claim — an
    /// operator revoked it, or it failed or landed — no settlement can ever be
    /// accepted for it, so the record settles locally with the refusal and
    /// releases its slot. Retrying it would refuse forever, and every pass
    /// would report that error instead of admitting new work. A claim the
    /// owner still holds keeps its settlement pending, as does a lost or
    /// uncertain delivery.
    fn deliver(&self, record: &LocalPullAdmission) -> Result<LocalPullAdmission, OrbitError> {
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
            } if claim.phase.is_unsettled() => return Err(refusal),
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
}

/// Largest failure excerpt a settlement carries. The whole diagnostic stays in
/// the executor's run record; the owner's reader needs enough to decide.
const MAX_FAILURE_EXCERPT_BYTES: usize = 8 * 1024;

/// The evidence a terminal leaf's failure settlement carries: its state and,
/// when the run recorded one, the error of its most recent failed step.
///
/// The owner cannot see an executor's run, and this summary becomes the
/// blocked task's `execution_summary`, so it names the host-local run and
/// quotes the failure rather than only saying that a handoff is missing.
#[cfg(test)]
pub(crate) fn terminal_failure_summary(run: &orbit_types::workflow::JobRun) -> String {
    terminal_failure_summary_with(run, None)
}

/// The failure settlement a terminal leaf implies, by how far its admission
/// got: a leaf that never launched was cancelled while queued, and a launched
/// one ended without the typed handoff success records.
///
/// Every follower process that settles a terminal leaf computes it here, so
/// the leaf's own worker, a cancel and a drain pass agree on the value.
/// `diagnostic` is the `(code, message)` the terminalizing caller knows before
/// its diagnostic step is durable.
pub(crate) fn leaf_failure_settlement(
    phase: LocalPullPhase,
    run: &orbit_types::workflow::JobRun,
    diagnostic: Option<(&str, &str)>,
) -> ClaimMutation {
    let summary = if matches!(phase, LocalPullPhase::Created | LocalPullPhase::Bound) {
        format!("queued leaf terminated as {} before launch", run.state)
    } else {
        terminal_failure_summary_with(run, diagnostic)
    };
    ClaimMutation::Fail(ClaimEvidence {
        summary: Some(summary),
        ..Default::default()
    })
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
            let mut cut = message.len().min(MAX_FAILURE_EXCERPT_BYTES);
            while cut > 0 && !message.is_char_boundary(cut) {
                cut -= 1;
            }
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
