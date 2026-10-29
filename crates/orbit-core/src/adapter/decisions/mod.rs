//! OpenAI Decisions translation, checked against the official create reference.
//! Only bounded text/JSON evidence is supported; no images, URLs or tool calls
//! are interpreted as executable input. No agent crew participates.

mod send_error;
mod wire;

use chrono::{DateTime, Utc};
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use orbit_config::assistance::AssistancePolicy;
use orbit_store::contracts::assistance::AssistanceClaim;
use orbit_types::assistance::{
    AssistanceLimits, AssistanceReason, AssistanceRequest, AssistanceResult,
};
use serde_json::Value;

use crate::application::assistance::{self, AssistanceAdapter};
use send_error::{DecisionsSendFailureClass, decisions_send_failure_reason};

pub const DECISIONS_ENDPOINT: &str = "https://api.openai.com/v1/decisions";

/// Secret source injected separately from evidence. Never implement Debug or
/// Serialize for credential-bearing implementations or transport requests.
pub trait AssistanceCredentialSource: Send + Sync {
    fn assistance_credential(&self, host_env_name: &str) -> Option<String>;
}

pub struct HostAssistanceCredential;

impl AssistanceCredentialSource for HostAssistanceCredential {
    fn assistance_credential(&self, host_env_name: &str) -> Option<String> {
        // This namespace is excluded from child_env even in explicit pass and
        // crew env lists. A workspace cannot redirect the reference.
        if !host_env_name.starts_with("ORBIT_ASSISTANCE_") {
            return None;
        }
        std::env::var(host_env_name)
            .ok()
            .filter(|s| !s.is_empty() && s.len() <= 8192)
    }
}

/// A fully received, bounded HTTP response. Bodies never enter the ledger.
pub struct AssistanceHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Supported synthetic transport seam. Implementations must return within the
/// requested deadline and enforce max_response_bytes while receiving data.
/// Errors must be fixed reason codes, never reqwest errors or response text.
pub trait DecisionsTransport: Send + Sync {
    fn send_assistance(
        &self,
        body: &Value,
        credential: &str,
        attempt_id: &str,
        deadline: DateTime<Utc>,
        limits: &AssistanceLimits,
    ) -> Result<AssistanceHttpResponse, AssistanceReason>;
}

pub struct OpenAiDecisionsTransport;

impl DecisionsTransport for OpenAiDecisionsTransport {
    fn send_assistance(
        &self,
        body: &Value,
        credential: &str,
        attempt_id: &str,
        deadline: DateTime<Utc>,
        limits: &AssistanceLimits,
    ) -> Result<AssistanceHttpResponse, AssistanceReason> {
        let remaining = (deadline - Utc::now())
            .to_std()
            .map_err(|_| AssistanceReason::TimeoutUnknown)?
            .min(Duration::from_secs(limits.timeout_seconds.into()));
        let client = reqwest::blocking::Client::builder()
            .timeout(remaining)
            .connect_timeout(remaining.min(Duration::from_secs(10)))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(true)
            .no_proxy()
            .build()
            .map_err(|_| AssistanceReason::Unavailable)?;
        let response = client
            .post(DECISIONS_ENDPOINT)
            .bearer_auth(credential)
            .header("X-Client-Request-Id", attempt_id)
            .json(body)
            .send()
            .map_err(|error| {
                let failure = if error.is_connect() {
                    DecisionsSendFailureClass::Connect
                } else if error.is_builder() {
                    DecisionsSendFailureClass::Builder
                } else if error.is_timeout() {
                    DecisionsSendFailureClass::Timeout
                } else {
                    DecisionsSendFailureClass::Other
                };
                decisions_send_failure_reason(failure)
            })?;
        let status = response.status().as_u16();
        if response
            .content_length()
            .is_some_and(|n| n > u64::from(limits.max_response_bytes))
        {
            return Err(AssistanceReason::Malformed);
        }
        let mut bytes = Vec::new();
        response
            .take(u64::from(limits.max_response_bytes) + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AssistanceReason::TimeoutUnknown)?;
        if bytes.len() > limits.max_response_bytes as usize {
            return Err(AssistanceReason::Malformed);
        }
        Ok(AssistanceHttpResponse {
            status,
            body: bytes,
        })
    }
}

pub struct OpenAiDecisionsAdapter {
    credentials: Arc<dyn AssistanceCredentialSource>,
    transport: Arc<dyn DecisionsTransport>,
}

impl Default for OpenAiDecisionsAdapter {
    fn default() -> Self {
        Self::with_assistance_transport(
            Arc::new(HostAssistanceCredential),
            Arc::new(OpenAiDecisionsTransport),
        )
    }
}

impl OpenAiDecisionsAdapter {
    /// Ordinary application boundary, also used by recorded response fixtures.
    /// Does not read credentials or send a request until called by the service.
    pub fn with_assistance_transport(
        credentials: Arc<dyn AssistanceCredentialSource>,
        transport: Arc<dyn DecisionsTransport>,
    ) -> Self {
        Self {
            credentials,
            transport,
        }
    }
}

impl AssistanceAdapter for OpenAiDecisionsAdapter {
    fn prepare_assistance_request(
        &self,
        request: AssistanceRequest,
        supplied_secrets: &[String],
        policy: &AssistancePolicy,
    ) -> Result<AssistanceRequest, AssistanceReason> {
        let mut secrets = supplied_secrets.to_vec();
        if let Some(credential) = self
            .credentials
            .assistance_credential(&policy.credential_env)
        {
            secrets.push(credential);
        }
        assistance::prepare_assistance_evidence(request, &secrets)
    }

    fn evaluate_assistance(
        &self,
        claim: &AssistanceClaim,
        policy: &AssistancePolicy,
    ) -> AssistanceResult {
        let Some(credential) = self
            .credentials
            .assistance_credential(&policy.credential_env)
        else {
            return AssistanceResult::assistance_gap(AssistanceReason::CredentialUnavailable);
        };
        // Rotation between enqueue and execution cannot send the new secret
        // back as evidence. Only this adapter resolves its value.
        let safe = match assistance::prepare_assistance_evidence(
            claim.work.request.clone(),
            std::slice::from_ref(&credential),
        ) {
            Ok(request) if request == claim.work.request => request,
            _ => return AssistanceResult::assistance_gap(AssistanceReason::ConfigurationChanged),
        };
        let body = match wire::assistance_decisions_body(
            &safe,
            &claim.work.requested_model,
            &claim.work.limits,
        ) {
            Ok(body) => body,
            Err(reason) => return AssistanceResult::assistance_gap(reason),
        };
        let started = Instant::now();
        let mut result = match self.transport.send_assistance(
            &body,
            &credential,
            &claim.attempt_id,
            claim.deadline,
            &claim.work.limits,
        ) {
            Ok(response)
                if response.body.len() <= claim.work.limits.max_response_bytes as usize =>
            {
                // Do not parse/store provider text containing credential values.
                let contains_secret =
                    std::str::from_utf8(&response.body).is_ok_and(|s| s.contains(&credential));
                if contains_secret {
                    AssistanceResult::assistance_gap(AssistanceReason::Malformed)
                } else {
                    wire::translate_assistance_decision(
                        response,
                        &safe,
                        &claim.work.requested_model,
                        &claim.work.model_revision,
                    )
                }
            }
            Ok(_) => AssistanceResult::assistance_gap(AssistanceReason::Malformed),
            Err(reason) => AssistanceResult::assistance_gap(reason),
        };
        result.latency_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        result
    }
}
