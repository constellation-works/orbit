//! Automatic adoption of an edited definition whose change cannot alter what
//! the retained debt means.
//!
//! Retuning a delivery auto-task — its threshold, wait, batch size, retries,
//! template, crew or dedupe — moves the definition's epoch, and once stalled
//! the consumer until an operator ran `recover --adopt-settings`. That fix was
//! always the same mechanical command, so the evaluator now runs it itself:
//! the same refusals, the same audited recovery record (attributed to
//! `system:automation`), every obligation retained. It warns once and files
//! one friction so the edit is not silent, then the pass continues.
//!
//! Everything that path refuses still fails closed as `definition_changed`,
//! now naming why. So does a consumer the automatic path cannot reason about
//! alone: one with no recorded trigger to name the change, or one already
//! stalled for an operator.

use super::{DeliveryHost, Evaluation, recovery};
use crate::AutomationError;
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{RecoveryRequest, SYSTEM_ACTOR, refusal};
use orbit_types::workflow::automation::{AutomationState, DeliveryTrigger};

/// What the host needs to file one deduped friction about an adoption.
pub struct AdoptionReport<'a> {
    pub consumer: &'a str,
    pub repository: &'a str,
    pub branch: &'a str,
    /// Identity the consumer recorded before the adoption.
    pub previous_epoch: &'a str,
    /// Identity it adopted.
    pub epoch: &'a str,
    /// Each setting that differs, by name.
    pub changes: &'a [String],
    pub at: DateTime<Utc>,
}

/// Where an edited definition leaves the evaluation pass.
pub(super) enum Adoption {
    /// The configured settings were adopted, or would be in a preview; the
    /// pass continues against this state.
    Adopted(Box<AutomationState>),
    /// The consumer keeps reporting `definition_changed`, for these reasons.
    /// Empty when the host does not adopt automatically at all.
    Refused(Vec<String>),
}

/// Everything that keeps the evaluator from adopting `trigger` over `state`
/// on its own; empty when it may. `repository` is the identity the source
/// reports for the configured branch now. `action_terminal` proves no action
/// is still executing, including a retry just scheduled by reconciliation.
///
/// Inspection calls this too, so it reports the same position a tick would
/// reach without writing anything.
pub fn refusals(
    store: &dyn AutomationStoreBackend,
    state: &AutomationState,
    epoch: &str,
    trigger: &DeliveryTrigger,
    repository: &str,
    action_terminal: bool,
) -> Result<Vec<String>, AutomationError> {
    // The automatic path always carries its own authorization, so only the
    // refusals about the consumer and the change itself can apply.
    let request = adopt_request("automatic settings adoption".into());
    let operation = recovery::Recovery {
        consumer: &state.consumer,
        epoch,
        trigger,
        repository,
        host_refusal: None,
        request: &request,
        by: SYSTEM_ACTOR,
        now: DateTime::<Utc>::UNIX_EPOCH,
        replay: None,
        resolved_action_id: None,
        expected_generation: Some(state.generation),
        action_terminal,
        action_failed_without_evidence: false,
    };

    let mut refusals = recovery::refusals(store, &operation, &request, state)?;
    let mut refuse = |reason: &str| {
        if !refusals.iter().any(|known| known == reason) {
            refusals.push(reason.to_string());
        }
    };

    if state.members.is_some() {
        refuse(refusal::MEMBER_CONSUMER);
    }

    // A frozen batch can prove the examination contract to an operator, but
    // only a recorded trigger names what changed, and an unnamed change is
    // not one the evaluator may judge compatible on its own.
    if state.trigger.is_none() {
        refuse(refusal::COVERAGE_UNVERIFIABLE);
    }

    // Adoption clears the stall marker, and clearing a stall is an operator's
    // decision.
    if state.stall.is_some() {
        refuse(refusal::CONSUMER_STALLED);
    }

    Ok(refusals)
}

/// Adopt the configured definition over `state` when nothing refuses it.
///
/// A preview projects the adopted identity without writing. A real pass
/// commits it through the operator recovery path, so a refusal or a lost
/// generation race changes nothing, then warns and reports exactly once: the
/// adopted epoch matches on every later tick.
pub(super) fn adopt(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &AutomationState,
    action_terminal: bool,
) -> Result<Adoption, AutomationError> {
    // Only the owner admits, and a disabled consumer probes no source.
    if !request.enabled || !host.adopts_settings() {
        return Ok(Adoption::Refused(vec![]));
    }

    // A changed branch is refused whatever the repository says, and its head
    // may not resolve at all.
    let repository = if state.branch == request.trigger.branch {
        host.repository(&request.trigger.branch)?
    } else {
        state.repository.clone()
    };

    let refused = refusals(
        store,
        state,
        request.epoch,
        request.trigger,
        &repository,
        action_terminal,
    )?;
    if !refused.is_empty() {
        return Ok(Adoption::Refused(refused));
    }

    if request.dry_run {
        let mut next = state.clone();
        next.epoch = request.epoch.into();
        next.trigger = Some(request.trigger.clone());
        return Ok(Adoption::Adopted(Box::new(next)));
    }

    let preview = RecoveryRequest::default();
    let changes = recovery::changes(
        state,
        &request_for(state, request, &repository, &preview, action_terminal),
    );
    let summary = format!(
        "settings changed ({}) — adopted automatically, coverage debt retained",
        changes.join(", ")
    );
    let adoption = adopt_request(summary.clone());
    recovery::apply(
        store,
        &request_for(state, request, &repository, &adoption, action_terminal),
    )?;

    let definition = request
        .consumer
        .rsplit('/')
        .next()
        .unwrap_or(request.consumer);
    tracing::warn!(
        consumer = request.consumer,
        previous_epoch = state.epoch,
        epoch = request.epoch,
        "{definition}: {summary}"
    );

    // The adoption is committed, and the next tick will not repeat it, so a
    // friction that cannot be filed is logged rather than failing the pass.
    if let Err(error) = host.report_adoption(&AdoptionReport {
        consumer: request.consumer,
        repository: &repository,
        branch: &request.trigger.branch,
        previous_epoch: &state.epoch,
        epoch: request.epoch,
        changes: &changes,
        at: request.now,
    }) {
        tracing::warn!(
            consumer = request.consumer,
            %error,
            "could not file the friction for an automatic settings adoption"
        );
    }

    store
        .automation_state(request.consumer)?
        .map(|state| Adoption::Adopted(Box::new(state)))
        .ok_or_else(|| AutomationError::Deferred("state_missing".into()))
}

/// An adopt-settings request carrying `reason` for the audit record.
fn adopt_request(reason: String) -> RecoveryRequest {
    RecoveryRequest {
        adopt_settings: true,
        reason,
        ..RecoveryRequest::default()
    }
}

fn request_for<'a>(
    state: &'a AutomationState,
    evaluation: &Evaluation<'a>,
    repository: &'a str,
    request: &'a RecoveryRequest,
    action_terminal: bool,
) -> recovery::Recovery<'a> {
    recovery::Recovery {
        consumer: &state.consumer,
        epoch: evaluation.epoch,
        trigger: evaluation.trigger,
        repository,
        host_refusal: None,
        request,
        by: SYSTEM_ACTOR,
        now: evaluation.now,
        replay: None,
        resolved_action_id: None,
        expected_generation: Some(state.generation),
        action_terminal,
        action_failed_without_evidence: false,
    }
}
