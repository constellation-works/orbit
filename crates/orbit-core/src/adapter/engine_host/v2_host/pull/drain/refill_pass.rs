//! The bounded refill pass: reconcile what is owed, then request new claims.
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{AdmissionReceipt, AdmissionRequest, PullDestination};

use super::{PullDrain, Reconciled, RefillPass};

/// Called with a claim's receipt and the time its request was allocated.
pub(crate) type Claimed<'a> =
    dyn Fn(&AdmissionReceipt, DateTime<Utc>) -> Result<(), OrbitError> + 'a;

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
    /// `claimed`, when given, is called with each claim's receipt and the
    /// time its request was allocated once the claim returns, before the next
    /// request's `template` is built: also when carrying the claim on to its
    /// leaf then fails. An error from it ends the pass.
    pub(crate) fn refill_pass_claiming(
        &self,
        destination: &PullDestination,
        template: &dyn Fn() -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        claimed: Option<&Claimed<'_>>,
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
                claimed,
                ceiling,
                &mut pass,
            )
            .err();
        pass
    }

    /// [`Self::refill_pass_claiming`] with no claim hook.
    #[cfg(test)]
    pub(crate) fn refill_pass(
        &self,
        destination: &PullDestination,
        template: &dyn Fn() -> Result<Option<AdmissionRequest>, OrbitError>,
        admitting: &dyn Fn() -> Result<bool, OrbitError>,
        ceiling: usize,
    ) -> RefillPass {
        self.refill_pass_claiming(destination, template, admitting, None, ceiling)
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
        claimed: Option<&Claimed<'_>>,
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
        let Some(mut next) = template()? else {
            return Ok(());
        };
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
            let requested_at = Utc::now();
            let Some(record) = self
                .jobs
                .allocate_pull_request(destination, &request, ceiling)?
            else {
                break;
            };
            let sent = claimed.map(|_| record.clone());
            let reconciled = self.reconcile(record);
            let reserved = match (claimed, sent) {
                (Some(claimed), Some(sent)) => self.reread(&sent).and_then(|current| match current
                    .and_then(|current| current.receipt)
                    .filter(|receipt| receipt.claim.is_some())
                {
                    Some(receipt) => claimed(&receipt, requested_at),
                    None => Ok(()),
                }),
                _ => Ok(()),
            };
            let reconciled = reconciled?;
            reserved?;
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
