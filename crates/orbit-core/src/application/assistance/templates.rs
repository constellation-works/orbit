//! Generic expansion of an exact host-granted template, without consumer rules.
use orbit_types::assistance::{AssistanceContract, AssistanceQuestion, AssistanceReason};

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
