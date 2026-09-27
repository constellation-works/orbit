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

pub(crate) struct PullDrain<'a> {
    pub(crate) jobs: &'a dyn JobRunStoreBackend,
    pub(crate) peer: &'a dyn PullPeer,
    pub(crate) launcher: &'a dyn PullLauncher,
}

impl PullDrain<'_> {
    /// One bounded refill. Each new ID is made durable before the request goes
    /// on the wire. An idle response ends the whole pass, regardless of free
    /// slots. Reconciliation errors prevent all fresh admission.
    pub(crate) fn refill(
        &self,
        destination: &PullDestination,
        template: &AdmissionRequest,
        ceiling: usize,
    ) -> Result<usize, OrbitError> {
        if !self.reconcile_pending(destination)? {
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
            ) && let Some(id) = record.leaf_run_id.as_deref()
            {
                let run = self.jobs.get_job_run(id)?.ok_or_else(|| {
                    OrbitError::Store("bound leaf disappeared; deliberate recovery required".into())
                })?;
                if run.state.is_terminal() {
                    record = self.update(
                        &record,
                        LocalPullMutation::Settle(Box::new(ClaimMutation::Fail(ClaimEvidence {
                            summary: Some(format!(
                                "queued leaf terminated as {} before launch",
                                run.state
                            )),
                            ..Default::default()
                        }))),
                    )?;
                }
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
                LocalPullPhase::Launched => {
                    let id = record
                        .leaf_run_id
                        .as_deref()
                        .ok_or_else(|| OrbitError::Store("launched leaf binding missing".into()))?;
                    let run = self.jobs.get_job_run(id)?.ok_or_else(|| {
                        OrbitError::Store(
                            "bound leaf disappeared; deliberate recovery required".into(),
                        )
                    })?;
                    if !run.state.is_terminal() {
                        return Ok(true);
                    }
                    // Success must have published a typed handoff before the
                    // run terminalized. Missing handoff is an execution failure.
                    self.update(
                        &record,
                        LocalPullMutation::Settle(Box::new(ClaimMutation::Fail(ClaimEvidence {
                            summary: Some(format!(
                                "leaf terminated as {} without acknowledged typed handoff",
                                run.state
                            )),
                            ..Default::default()
                        }))),
                    )?
                }
                LocalPullPhase::Settling => self.deliver(&record)?,
                LocalPullPhase::Settled | LocalPullPhase::Refused => return Ok(true),
                LocalPullPhase::Idle => return Ok(false),
            };
        }
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
