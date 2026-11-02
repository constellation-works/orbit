//! Generic expansion of an exact host-granted template, without consumer rules.
use orbit_types::assistance::{
    AssistanceContract, AssistanceEvidencePacket, AssistancePacketBoundChoice, AssistanceQuestion,
    AssistanceReason, AssistanceRequest, assistance_identifier,
};
use serde_json::Value;

pub(super) fn validate_assistance_expansion(
    expanded: &AssistanceContract,
    template: &AssistanceContract,
) -> Result<(), AssistanceReason> {
    expanded.validate_contract()?;
    template.validate_contract()?;
    let invalid = AssistanceReason::InvalidRequest;
    let mut normalized = expanded.clone();
    normalized.questions = template.questions.clone();
    if &normalized != template
        || template
            .questions
            .values()
            .any(|q| !q.criterion_refs().is_empty())
    {
        return Err(invalid);
    }
    for (name, question) in &expanded.questions {
        // A template name or name/<positive index>[/<positive index>] is the
        // entire naming language; no arbitrary model-selected rubric text.
        let mut parts = name.split('/');
        let base = parts.next().ok_or(invalid)?;
        let suffix: Vec<_> = parts.collect();
        if suffix.len() > 2
            || suffix.iter().any(|s| {
                s.parse::<u32>()
                    .ok()
                    .is_none_or(|n| n == 0 || n.to_string() != *s)
            })
        {
            return Err(invalid);
        }
        let mut normalized_question = question.clone();
        match &mut normalized_question {
            AssistanceQuestion::Predicate { criterion_refs, .. }
            | AssistanceQuestion::Choice { criterion_refs, .. } => criterion_refs.clear(),
        }
        if template.questions.get(base) != Some(&normalized_question) {
            return Err(invalid);
        }
    }
    Ok(())
}

pub(super) fn contract_has_packet_bound_choice(contract: &AssistanceContract) -> bool {
    contract.questions.values().any(|question| {
        matches!(
            question,
            AssistanceQuestion::Choice {
                packet_bound: Some(_),
                ..
            }
        )
    })
}

/// Copy each packet-bound choice's option ids onto `allowed`, then clear the
/// binding. The granted template is unchanged, so its digest still covers the
/// field name, bounds and abstention rather than the per-request ids. The
/// stored question is an ordinary choice over the copied set.
pub(super) fn bind_packet_bound_choices(
    request: &mut AssistanceRequest,
) -> Result<(), AssistanceReason> {
    let names: Vec<String> = request
        .contract
        .questions
        .iter()
        .filter(|(_, question)| {
            matches!(
                question,
                AssistanceQuestion::Choice {
                    packet_bound: Some(_),
                    ..
                }
            )
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in names {
        let (bound, abstain) = match request.contract.questions.get(&name) {
            Some(AssistanceQuestion::Choice {
                abstain_values,
                packet_bound: Some(bound),
                ..
            }) => (
                bound.clone(),
                abstain_values
                    .first()
                    .cloned()
                    .ok_or(AssistanceReason::InvalidRequest)?,
            ),
            _ => return Err(AssistanceReason::InvalidRequest),
        };
        let mut allowed = packet_bound_option_ids(&request.evidence_packet, &bound, &abstain)?;
        allowed.push(abstain);
        let Some(AssistanceQuestion::Choice {
            allowed: slot,
            packet_bound,
            ..
        }) = request.contract.questions.get_mut(&name)
        else {
            return Err(AssistanceReason::InvalidRequest);
        };
        *slot = allowed;
        *packet_bound = None;
    }
    request.contract.validate_contract()
}

fn packet_bound_option_ids(
    packet: &AssistanceEvidencePacket,
    bound: &AssistancePacketBoundChoice,
    abstain: &str,
) -> Result<Vec<String>, AssistanceReason> {
    let Some(items) = packet.fields.get(&bound.field).and_then(Value::as_array) else {
        return Err(AssistanceReason::InvalidRequest);
    };
    if items.is_empty() || items.len() > usize::from(bound.max_ids) {
        return Err(AssistanceReason::InvalidRequest);
    }
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let Some(id) = item.as_str() else {
            return Err(AssistanceReason::InvalidRequest);
        };
        if id.len() > usize::from(bound.max_id_bytes)
            || !assistance_identifier(id)
            || id == abstain
            || ids.iter().any(|seen| seen == id)
        {
            return Err(AssistanceReason::InvalidRequest);
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}
