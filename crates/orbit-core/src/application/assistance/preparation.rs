use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_config::assistance::{AssistancePolicy, AssistanceSettings};
use orbit_store::contracts::assistance::AssistanceWork;
use orbit_types::assistance::{self, AssistanceReason, AssistanceRequest};

use super::AssistanceAdapter;

pub(super) fn assistance_policy_digest(
    settings: &AssistanceSettings,
    policy: &AssistancePolicy,
) -> Result<String, OrbitError> {
    serde_json::to_vec(&(policy, &settings.limits, &settings.budget))
        .map(|v| assistance::assistance_digest(&v))
        .map_err(|_| OrbitError::InvalidInput("cannot encode assistance policy".into()))
}

/// Typed conversion around the shared external-boundary redactor.
pub(crate) fn prepare_assistance_evidence(
    request: AssistanceRequest,
    secrets: &[String],
) -> Result<AssistanceRequest, AssistanceReason> {
    let value = serde_json::to_value(request).map_err(|_| AssistanceReason::InvalidRequest)?;
    serde_json::from_value(
        orbit_common::security::redaction::redact_secret_values_json(value, secrets),
    )
    .map_err(|_| AssistanceReason::InvalidRequest)
}

pub(super) fn prepare_assistance_work(
    mut request: AssistanceRequest,
    secrets: &[String],
    policy: &AssistancePolicy,
    settings: &AssistanceSettings,
    adapter: &dyn AssistanceAdapter,
    now: DateTime<Utc>,
    template: Option<&assistance::AssistanceContract>,
) -> Result<AssistanceWork, AssistanceReason> {
    request.validate_request()?;
    if secrets.len() > 128
        || secrets.iter().any(|s| s.len() > 65_536)
        || request.evidence_packet.collected_at > now
        || now - request.evidence_packet.collected_at
            > chrono::Duration::seconds(settings.limits.queue_age_seconds.into())
    {
        return Err(AssistanceReason::InvalidRequest);
    }
    let serialized = serde_json::to_vec(&request).map_err(|_| AssistanceReason::InvalidRequest)?;
    // Refuse oversize inputs instead of silently cutting evidence mid-field.
    if serialized.len() > settings.limits.max_request_bytes as usize {
        return Err(AssistanceReason::InvalidRequest);
    }
    let original_fields = request.evidence_packet.fields.len();
    request
        .evidence_packet
        .fields
        .retain(|key, _| policy.evidence_fields.contains(key));
    let original_refs = request.evidence_packet.evidence_refs.len();
    request.evidence_packet.evidence_refs.retain(|r| {
        policy
            .evidence_ref_prefixes
            .iter()
            .any(|p| r.starts_with(p))
    });
    if original_fields != request.evidence_packet.fields.len()
        || original_refs != request.evidence_packet.evidence_refs.len()
    {
        request
            .evidence_packet
            .omissions
            .push("host_scope_omissions".into());
        request.evidence_packet.truncated = true;
    }
    request = adapter.prepare_assistance_request(request, secrets, policy)?;
    request.validate_request()?;
    if template.is_none() && super::templates::contract_has_packet_bound_choice(&request.contract) {
        return Err(AssistanceReason::InvalidRequest);
    }
    if let Some(template) = template {
        super::templates::validate_assistance_expansion(&request.contract, template)?;
        super::templates::bind_packet_bound_choices(&mut request)?;
    }
    let contract = serde_json::to_vec(template.unwrap_or(&request.contract))
        .map_err(|_| AssistanceReason::InvalidRequest)?;
    if assistance::assistance_digest(&contract) != policy.contract_digest {
        return Err(AssistanceReason::InvalidRequest);
    }
    let payload = serde_json::to_vec(&request).map_err(|_| AssistanceReason::InvalidRequest)?;
    if payload.len() > settings.limits.max_request_bytes as usize {
        return Err(AssistanceReason::InvalidRequest);
    }
    let evidence_digest = assistance::assistance_digest(
        &serde_json::to_vec(&request.evidence_packet)
            .map_err(|_| AssistanceReason::InvalidRequest)?,
    );
    let policy_digest =
        assistance_policy_digest(settings, policy).map_err(|_| AssistanceReason::InvalidRequest)?;
    let slot = assistance::assistance_digest(
        &serde_json::to_vec(&(
            &request.workspace_id,
            &request.owner_id,
            &request.use_case,
            &request.evidence_packet.subject_id,
        ))
        .map_err(|_| AssistanceReason::InvalidRequest)?,
    );
    let id = assistance::assistance_digest(
        &serde_json::to_vec(&(
            &slot,
            &policy_digest,
            &request.source_digest,
            &evidence_digest,
        ))
        .map_err(|_| AssistanceReason::InvalidRequest)?,
    );
    Ok(AssistanceWork {
        id,
        slot,
        policy_digest,
        evidence_digest,
        request,
        adapter: adapter.assistance_adapter_identity().into(),
        requested_model: policy.model.clone(),
        model_revision: policy.model_revision.clone(),
        mode: policy.mode,
        created_at: now,
        limits: settings.limits.clone(),
        budget: settings.budget.clone(),
    })
}
