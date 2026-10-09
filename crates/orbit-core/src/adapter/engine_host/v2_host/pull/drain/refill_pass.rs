//! The bounded refill pass: reconcile what is owed, then request new claims.
use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionReceipt, AdmissionRequest, PullDestination};

use super::{CONSECUTIVE_FAILURE_BREAKER, PullDrain, Reconciled, RefillPass};

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
}
