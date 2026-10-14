//! The bounded refill pass: reconcile what is owed, then request new claims.
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionReceipt, AdmissionRequest, PullDestination};

use super::{PullDrain, Reconciled, RefillPass};

/// Called after every allocated request, with its receipt when available.
pub(crate) type Requested<'a> =
    dyn Fn(Option<&AdmissionReceipt>, DateTime<Utc>) -> Result<(), OrbitError> + 'a;

impl PullDrain<'_> {
    /// One bounded refill. Each new ID is made durable before the request goes
    /// on the wire. An idle response ends the whole pass, regardless of free
    /// slots. Reconciliation errors prevent all fresh admission.
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
    /// [ORB-14174]; an error from it ends the pass too. It is first asked
    /// after reconciliation, so the failure breaker it checks sees a leaf
    /// this pass just settled as failed: that pass must not request
    /// replacements [ORB-15639].
    ///
    /// `requested`, when given, observes every allocated request, including
    /// idle responses and failures, before the next template is built. Its
    /// receipt can contain a claim even if carrying it to a leaf failed.
    /// An error from the hook ends the pass.
    pub(crate) fn refill_pass_requesting(
        &self,
        destination: &PullDestination,
        template: &dyn Fn(&str) -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        requested: Option<&Requested<'_>>,
        ceiling: usize,
    ) -> RefillPass {
        let mut pass = RefillPass {
            admitted: 0,
            error: None,
            answer: None,
        };
        pass.error = self
            .refill_into(
                destination,
                template,
                admitting,
                requested,
                ceiling,
                &mut pass,
            )
            .err();
        pass
    }

    /// [`Self::refill_pass_requesting`] with no claim hook.
    #[cfg(test)]
    pub(crate) fn refill_pass(
        &self,
        destination: &PullDestination,
        template: &dyn Fn() -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        ceiling: usize,
    ) -> RefillPass {
        self.refill_pass_requesting(destination, &|_| template(), admitting, None, ceiling)
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
        template: &dyn Fn(&str) -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        requested: Option<&Requested<'_>>,
        ceiling: usize,
        pass: &mut RefillPass,
    ) -> Result<(), OrbitError> {
        match self.reconcile_pending_answer(destination)? {
            Reconciled::Open => {}
            Reconciled::Held => return Ok(()),
            Reconciled::Idle(receipt) => {
                pass.answer = Some(receipt);
                return Ok(());
            }
        }
        for _ in 0..ceiling {
            if !admitting()? {
                break;
            }
            // Allocate identity before fitting, so capacity can be held in
            // the same host transaction that computes the request's fit.
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|error| {
                OrbitError::Execution(format!("allocate pull request identity: {error}"))
            })?;
            let request_id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            // Rebuild every time: earlier leaves can change crew exclusions
            // and the host forecast before this request is made.
            let Some(mut request) = template(&request_id)? else {
                break;
            };
            request.request_id = request_id;
            let requested_at = Utc::now();
            let record = match self
                .jobs
                .allocate_pull_request(destination, &request, ceiling)
            {
                Ok(Some(record)) => record,
                Ok(None) => {
                    self.peer.discard(&request)?;
                    break;
                }
                Err(error) => {
                    self.peer.discard(&request)?;
                    return Err(error);
                }
            };
            let sent = requested.map(|_| record.clone());
            let reconciled = self.reconcile(record);
            let observed = match (requested, sent) {
                (Some(requested), Some(sent)) => {
                    let current = self.reread(&sent);
                    let observed = requested(
                        current
                            .as_ref()
                            .ok()
                            .and_then(|current| current.as_ref())
                            .and_then(|current| current.receipt.as_ref()),
                        requested_at,
                    );
                    current?;
                    observed
                }
                _ => Ok(()),
            };
            let reconciled = reconciled?;
            observed?;
            match reconciled {
                Reconciled::Open => pass.admitted += 1,
                Reconciled::Held => break,
                Reconciled::Idle(receipt) => {
                    pass.answer = Some(receipt);
                    break;
                }
            }
        }
        Ok(())
    }
}
