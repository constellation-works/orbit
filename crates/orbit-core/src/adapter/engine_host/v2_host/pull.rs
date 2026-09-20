//! Internal pull refill loop. Public invocation stays gated until lifecycle
//! integration supplies the trusted destination and executable leaf adapter.
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionReceipt, AdmissionRequest, ClaimEvidence, ClaimMutation, JobRunStoreBackend,
    LocalPullAdmission, LocalPullMutation, LocalPullPhase, PullDestination,
};

/// Trusted owner transport, supplied by runtime composition. Implementations
/// must check current claim/run/phase on bind and settlement; replaying a receipt
/// never supplies execution authority. There is no local fallback.
#[allow(dead_code)]
pub(crate) trait PullPeer {
    fn request(
        &self,
        destination: &PullDestination,
        request: &AdmissionRequest,
    ) -> Result<AdmissionReceipt, OrbitError>;
    fn bind(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
    fn settle(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
}

/// A launcher takes the existing bound run, never submits a replacement.
/// Success means that launch was acknowledged, not that execution completed.
#[allow(dead_code)]
pub(crate) trait PullLauncher {
    fn launch(&self, admission: &LocalPullAdmission) -> Result<(), OrbitError>;
}

#[allow(dead_code)]
pub(crate) struct PullDrain<'a> {
    pub(crate) jobs: &'a dyn JobRunStoreBackend,
    pub(crate) peer: &'a dyn PullPeer,
    pub(crate) launcher: &'a dyn PullLauncher,
}

#[allow(dead_code)]
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
        for record in self.jobs.local_pull_admissions()? {
            if record.destination != *destination {
                continue;
            }
            if !self.reconcile(record)? {
                return Ok(0);
            }
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

    fn update(
        &self,
        record: &LocalPullAdmission,
        mutation: LocalPullMutation,
    ) -> Result<LocalPullAdmission, OrbitError> {
        self.jobs
            .mutate_local_pull(&record.destination, &record.request.request_id, &mutation)
    }

    /// Returns false only for a newly reconciled idle receipt. Historical idle
    /// records are skipped so the next polling pass can allocate a fresh ID.
    fn reconcile(&self, mut record: LocalPullAdmission) -> Result<bool, OrbitError> {
        if matches!(record.phase, LocalPullPhase::Idle | LocalPullPhase::Settled) {
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
                LocalPullPhase::Requested => {
                    let receipt = self.peer.request(&record.destination, &record.request)?;
                    self.update(&record, LocalPullMutation::Receive(Box::new(receipt)))?
                }
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
                        self.peer.settle(&record)?;
                        self.update(&record, LocalPullMutation::Settled)?;
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
                LocalPullPhase::Settling => {
                    self.peer.settle(&record)?;
                    self.update(&record, LocalPullMutation::Settled)?
                }
                LocalPullPhase::Settled => return Ok(true),
                LocalPullPhase::Idle => return Ok(false),
            };
        }
    }

    /// Called by the leaf terminal hook even after parent admission has stopped.
    /// Persist first: a disconnect leaves exactly this immutable settlement for
    /// a later refill or explicit reconciliation to retry idempotently.
    pub(crate) fn settle(
        &self,
        record: &LocalPullAdmission,
        settlement: ClaimMutation,
    ) -> Result<(), OrbitError> {
        let pending = self.update(record, LocalPullMutation::Settle(Box::new(settlement)))?;
        self.peer.settle(&pending)?;
        self.update(&pending, LocalPullMutation::Settled)?;
        Ok(())
    }
}
