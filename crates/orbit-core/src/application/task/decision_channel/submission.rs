use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_TOKEN_CONSUMED_EVENT, DecisionGate, DecisionRequest,
    DecisionRequestState, TaskHistoryEntry,
};

use crate::OrbitRuntime;
use crate::application::task::helpers::SYSTEM_ACTOR_LABEL;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

use super::dkim::{
    DkimTxtResolver, DkimVerifyError, SystemDkimTxtResolver, sender_domain_of, verify_dkim,
};
use super::model::{DecisionSubmitCaller, DecisionSubmitParams, MAX_DECISION_RAW_MESSAGE_BYTES};
use super::registration::ChannelRegistration;
use super::token;

fn answer_not_allowed(request: &DecisionRequest, why: &str) -> OrbitError {
    let allowed = request
        .answers
        .iter()
        .map(|answer| answer.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    OrbitError::decision_refused(
        DecisionRefusalCode::AnswerNotAllowed,
        format!(
            "{why} for decision request `{}` (allowed: {allowed}); the token was not consumed and nothing was applied",
            request.id
        ),
    )
}

/// The channel binding and transport evidence design §3.3 step 4
/// verifies: the calling plugin is the channel's bound plugin, a plugin
/// submission presents an allowlisted sender, and the forwarded
/// `Authentication-Results` passes. The operator's own test submission
/// may omit sender and evidence; a registered allowlist applies to any
/// sender presented. Judged before the DKIM lookup and again under the
/// store lock, so a revoke or rotation between them still refuses.
fn check_transport_evidence(
    params: &DecisionSubmitParams,
    registration: &ChannelRegistration,
) -> Result<(), OrbitError> {
    if let DecisionSubmitCaller::PluginCallback { plugin } = &params.caller
        && plugin != &registration.plugin
    {
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::ChannelNotRegistered,
            format!(
                "decision channel `{}` is not bound to plugin `{plugin}`; nothing was applied",
                params.channel
            ),
        ));
    }
    let transport = matches!(params.caller, DecisionSubmitCaller::PluginCallback { .. });
    let presented = params
        .sender
        .as_deref()
        .map(|sender| sender.trim().to_ascii_lowercase())
        .filter(|sender| !sender.is_empty());
    if transport && presented.is_none() {
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::SenderNotAllowed,
            format!(
                "the transport presented no sender for decision channel `{}`; nothing was applied",
                params.channel
            ),
        ));
    }
    if let Some(sender) = presented.as_deref()
        && !registration.allowed_senders.is_empty()
        && !registration
            .allowed_senders
            .iter()
            .any(|allowed| allowed == sender)
    {
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::SenderNotAllowed,
            format!(
                "the presented sender is not on decision channel `{}`'s allowlist; nothing was applied",
                params.channel
            ),
        ));
    }
    let evidence_passes = match params.auth_results.as_deref() {
        Some(auth_results) => authentication_results_pass(auth_results),
        None => !transport,
    };
    if !evidence_passes {
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::AuthenticationFailed,
            "the transport did not forward Authentication-Results showing dkim=pass and spf=pass without dmarc=fail; nothing was applied",
        ));
    }
    Ok(())
}

fn authentication_results_pass(header: &str) -> bool {
    let header = header.to_ascii_lowercase();
    header.contains("dkim=pass") && header.contains("spf=pass") && !header.contains("dmarc=fail")
}

impl OrbitRuntime {
    /// Submit a human's answer through a registered channel, applying it
    /// in core as the registered human when — and only when — every
    /// check passes, in the design's order: the caller is the channel's
    /// bound plugin (or the operator testing it), the channel is
    /// registered and unrevoked, the presented sender and authentication
    /// evidence pass, the request exists, the token verifies under the
    /// current epoch and is unconsumed, the answer is allowed, and the
    /// request is still open against its binding. Each refusal names
    /// itself (`caller_not_allowed`, `channel_not_registered`,
    /// `channel_revoked`, `sender_not_allowed`, `authentication_failed`,
    /// `request_not_found`, `token_invalid`, `token_consumed`,
    /// `answer_not_allowed`, `request_expired`, `request_superseded`)
    /// and applies nothing. When the submission carries a raw message,
    /// core also verifies its DKIM signature itself before any of that
    /// and refuses `dkim_verification_failed` when its own result
    /// contradicts the forwarded verdicts, or
    /// `dkim_verification_unavailable` (retryable) when the DNS lookup
    /// fails; a channel registered to require that verification refuses
    /// a submission without a raw message as
    /// `dkim_verification_required`. None of these consumes the token.
    pub fn submit_decision_via_channel(
        &self,
        params: DecisionSubmitParams,
    ) -> Result<DecisionRequest, OrbitError> {
        self.submit_decision_via_channel_with_resolver(params, &SystemDkimTxtResolver::default())
    }

    /// [`OrbitRuntime::submit_decision_via_channel`] with the DKIM DNS
    /// resolver supplied by the caller, so tests verify against a fixture
    /// key and never touch live DNS.
    pub fn submit_decision_via_channel_with_resolver(
        &self,
        params: DecisionSubmitParams,
        dkim_resolver: &dyn DkimTxtResolver,
    ) -> Result<DecisionRequest, OrbitError> {
        match &params.caller {
            DecisionSubmitCaller::PluginCallback { .. } | DecisionSubmitCaller::Operator => {}
            DecisionSubmitCaller::Agent | DecisionSubmitCaller::Unknown => {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::CallerNotAllowed,
                    "submitting a decision is a registered channel's or the operator's; an agent session never answers for a human",
                ));
            }
        }
        if !self.decision_channel_enabled() {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::DecisionChannelDisabled,
                "the decision channel is not enabled in this workspace; nothing was applied",
            ));
        }
        // A forwarded raw message is DKIM-verified by core itself,
        // before the store lock below is taken: the DNS lookup has a
        // bounded timeout and must never block revoke or rotate. The
        // non-DNS evidence is judged first (and again under the lock),
        // so the existing refusal precedence is unchanged.
        if let Some(raw_message) = params
            .raw_message
            .as_deref()
            .filter(|message| !message.is_empty())
        {
            if raw_message.len() > MAX_DECISION_RAW_MESSAGE_BYTES {
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::RawMessageTooLarge,
                    format!(
                        "the forwarded raw message is {} bytes, over the {}-byte bound for core-side DKIM verification; nothing was applied",
                        raw_message.len(),
                        MAX_DECISION_RAW_MESSAGE_BYTES
                    ),
                ));
            }
            let preliminary = self.active_registration(&params.channel)?;
            check_transport_evidence(&params, &preliminary)?;
            let sender_domain = params
                .sender
                .as_deref()
                .and_then(sender_domain_of)
                .or_else(|| {
                    preliminary
                        .allowed_senders
                        .first()
                        .and_then(|sender| sender_domain_of(sender))
                });
            verify_dkim(
                raw_message.as_bytes(),
                sender_domain.as_deref(),
                dkim_resolver,
            )
            .map_err(|error| match error {
                DkimVerifyError::Failed(reason) => OrbitError::decision_refused(
                    DecisionRefusalCode::DkimVerificationFailed,
                    format!("{reason}; nothing was applied"),
                ),
                DkimVerifyError::Unavailable(reason) => OrbitError::decision_refused(
                    DecisionRefusalCode::DkimVerificationUnavailable,
                    format!(
                        "retryable: {reason}; the token was not consumed and nothing was applied"
                    ),
                ),
            })?;
        }
        // Held from the registration read through the apply: a revoke or
        // rotation cannot land between the check and the apply, and two
        // presentations of one token must not both pass the single-use check.
        // Nothing this path reaches takes the same lock.
        let _submission = self.lock_decision_channel_store()?;
        let registration = self.active_registration(&params.channel)?;
        check_transport_evidence(&params, &registration)?;
        if registration.require_dkim_verification
            && params
                .raw_message
                .as_deref()
                .is_none_or(|message| message.is_empty())
        {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::DkimVerificationRequired,
                format!(
                    "decision channel `{}` requires core-side DKIM verification, so the transport must forward the raw message; nothing was applied",
                    params.channel
                ),
            ));
        }
        let request = self
            .get_decision_request(&params.request_id)?
            .ok_or_else(|| {
                OrbitError::decision_refused(
                    DecisionRefusalCode::RequestNotFound,
                    format!(
                        "no decision request `{}` in this workspace; nothing was applied",
                        params.request_id
                    ),
                )
            })?;
        let token = params.token.trim().to_ascii_uppercase();
        let fingerprint = token::sha256_hex(token.as_bytes());
        if !token::token_well_formed(&token)
            || !token::token_matches(&token, &registration.key_hex, registration.epoch, &request)?
        {
            if token::token_well_formed(&token)
                && let (Some(epoch), Some(key)) = (
                    registration.previous_epoch,
                    registration.previous_key_hex.as_deref(),
                )
                && token::token_matches(&token, key, epoch, &request)?
            {
                self.consume_decision_token(&request, &fingerprint, "key_rotated")?;
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::TokenConsumed,
                    format!(
                        "the token for decision request `{}` was minted under key epoch {epoch}, which rotation invalidated; nothing was applied",
                        request.id
                    ),
                ));
            }
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::TokenInvalid,
                format!(
                    "the token does not verify for decision request `{}` under channel `{}`; nothing was applied",
                    request.id, params.channel
                ),
            ));
        }
        if self.decision_token_consumed(&request, &fingerprint)? {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::TokenConsumed,
                format!(
                    "the token for decision request `{}` was already presented and judged; the original outcome stands and nothing was applied again",
                    request.id
                ),
            ));
        }
        let answer_id = params.answer_id.trim();
        if answer_id.is_empty() {
            return Err(answer_not_allowed(&request, "the reply carried no answer"));
        }
        if !request.allows(answer_id) {
            return Err(answer_not_allowed(
                &request,
                &format!("`{answer_id}` is not an allowed answer"),
            ));
        }
        let note = params
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty());
        if request.gate == DecisionGate::PilotAdmission
            && answer_id == "evaluated"
            && note.is_none()
        {
            return Err(answer_not_allowed(
                &request,
                "an `evaluated` verdict needs its evaluation as the note",
            ));
        }
        match request.state {
            DecisionRequestState::Open => {}
            DecisionRequestState::Expired => {
                self.consume_decision_token(&request, &fingerprint, "request_expired")?;
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::RequestExpired,
                    format!(
                        "decision request `{}` expired at {}; nothing was applied",
                        request.id,
                        request.expires_at.to_rfc3339()
                    ),
                ));
            }
            DecisionRequestState::Answered | DecisionRequestState::Superseded => {
                self.consume_decision_token(&request, &fingerprint, "request_superseded")?;
                return Err(OrbitError::decision_refused(
                    DecisionRefusalCode::RequestSuperseded,
                    format!(
                        "decision request `{}` no longer matches the hold on task {}; nothing was applied",
                        request.id, request.task_id
                    ),
                ));
            }
        }
        let provenance = format!(
            "decision-channel {} (request {}, token sha256:{fingerprint})",
            registration.channel, request.id
        );
        self.answer_decision_request_as_human(
            &request.id,
            answer_id,
            note.map(str::to_string),
            registration.human.clone(),
            Some(provenance),
        )
    }

    /// Whether `fingerprint` already judged a submission for `request`:
    /// the applied answer's provenance carries it, or a post-authentication
    /// refusal consumed it.
    fn decision_token_consumed(
        &self,
        request: &DecisionRequest,
        fingerprint: &str,
    ) -> Result<bool, OrbitError> {
        let history = self.get_task_history(&request.task_id)?;
        Ok(history.iter().any(|entry| {
            (entry.event == DECISION_TOKEN_CONSUMED_EVENT || entry.event == DECISION_ANSWERED_EVENT)
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains(fingerprint))
        }))
    }

    fn consume_decision_token(
        &self,
        request: &DecisionRequest,
        fingerprint: &str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        let entry = TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: DECISION_TOKEN_CONSUMED_EVENT.to_string(),
            note: Some(
                serde_json::json!({
                    "request_id": request.id,
                    "token_sha256": fingerprint,
                    "reason": reason,
                })
                .to_string(),
            ),
            from_status: None,
            to_status: None,
            operation_id: Some(request.id.clone()),
            decision: None,
        };
        self.stores().task_records().update(
            &request.task_id,
            TaskRecordUpdateParams {
                actor: SYSTEM_ACTOR_LABEL.to_string(),
                append_history: vec![entry],
                ..Default::default()
            },
        )?;
        Ok(())
    }
}
