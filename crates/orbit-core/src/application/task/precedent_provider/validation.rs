//! Reject a provider response atomically, without dropping or repairing rows.

use std::collections::BTreeSet;

use orbit_types::identity::is_automation_actor;
use serde_json::Value;

use super::model::*;

pub fn validate_precedent_request(
    request: &PrecedentProviderRequest,
) -> Result<(), PrecedentGapReason> {
    if request.contract != PRECEDENT_PROVIDER_CONTRACT {
        return Err(PrecedentGapReason::ContractMismatch);
    }
    let encoded = serde_json::to_vec(request).map_err(|_| PrecedentGapReason::InvalidShape)?;
    if encoded.len() > MAX_PRECEDENT_REQUEST_BYTES
        || request.k == 0
        || request.k > MAX_PRECEDENT_K
        || !precedent_id_valid(&request.workspace_id)
        || !precedent_id_valid(&request.request.id)
        || !precedent_id_valid(&request.request.gate)
        || !precedent_id_valid(&request.request.category)
        || request.request.question.trim().is_empty()
        || request.as_of != request.request.raised_at
    {
        return Err(PrecedentGapReason::Bounds);
    }
    validate_precedent_strings(
        &serde_json::to_value(request).map_err(|_| PrecedentGapReason::InvalidShape)?,
    )?;
    validate_precedent_options(&request.request.options)
}

/// Validate the original JSON bytes before deserializing any record. Both
/// built-in and plugin sources enter this same whole-response boundary.
pub fn validate_precedent_response(
    request: &PrecedentProviderRequest,
    bytes: &[u8],
) -> Result<PrecedentProviderResponse, PrecedentGapReason> {
    validate_precedent_request(request)?;
    if bytes.len() > MAX_PRECEDENT_RESPONSE_BYTES {
        return Err(PrecedentGapReason::Bounds);
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| PrecedentGapReason::InvalidShape)?;
    validate_precedent_strings(&value)?;
    let response: PrecedentProviderResponse =
        serde_json::from_value(value).map_err(|_| PrecedentGapReason::InvalidShape)?;
    if response.contract != PRECEDENT_PROVIDER_CONTRACT {
        return Err(PrecedentGapReason::ContractMismatch);
    }
    if response.precedents.len() > request.k || response.omissions.len() > 32 {
        return Err(PrecedentGapReason::Bounds);
    }
    let mut ids = BTreeSet::new();
    for precedent in &response.precedents {
        if !ids.insert(&precedent.id) {
            return Err(PrecedentGapReason::DuplicateId);
        }
        if precedent.decided_at >= request.as_of {
            return Err(PrecedentGapReason::FutureDecision);
        }
        validate_precedent_options(&precedent.options)?;
        if !precedent
            .options
            .iter()
            .any(|option| option.id == precedent.chosen_option)
        {
            return Err(PrecedentGapReason::UnknownOption);
        }
        if !precedent_human_actor(&precedent.decided_by) {
            return Err(PrecedentGapReason::AutomationActor);
        }
        if precedent.scope.workspace_id != request.workspace_id {
            return Err(PrecedentGapReason::WorkspaceMismatch);
        }
        if !precedent_id_valid(&precedent.id)
            || !precedent_id_valid(&precedent.scope.category)
            || !precedent_id_valid(&precedent.citation.source)
            || precedent.question.trim().is_empty()
            || precedent.citation.reference.trim().is_empty()
            || !orbit_types::assistance::assistance_is_digest(&precedent.citation.digest)
        {
            return Err(PrecedentGapReason::InvalidShape);
        }
    }
    Ok(response)
}

pub(super) fn precedent_human_actor(actor: &str) -> bool {
    let actor = actor.trim();
    !actor.trim().is_empty()
        && actor != "model"
        && actor != "precedent"
        && !is_automation_actor(actor)
}

fn precedent_id_valid(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn validate_precedent_options(options: &[PrecedentOption]) -> Result<(), PrecedentGapReason> {
    let mut ids = BTreeSet::new();
    if options.is_empty()
        || options.len() > 8
        || options.iter().any(|option| {
            !precedent_id_valid(&option.id)
                || option.id.len() > 64
                || option.label.trim().is_empty()
                || !ids.insert(&option.id)
        })
    {
        return Err(PrecedentGapReason::Bounds);
    }
    Ok(())
}

fn validate_precedent_strings(value: &Value) -> Result<(), PrecedentGapReason> {
    match value {
        Value::String(text) if text.len() > MAX_PRECEDENT_STRING_BYTES => {
            Err(PrecedentGapReason::Bounds)
        }
        Value::Array(items) => items.iter().try_for_each(validate_precedent_strings),
        Value::Object(fields) => fields.iter().try_for_each(|(key, value)| {
            if key.len() > MAX_PRECEDENT_STRING_BYTES {
                return Err(PrecedentGapReason::Bounds);
            }
            validate_precedent_strings(value)
        }),
        _ => Ok(()),
    }
}
