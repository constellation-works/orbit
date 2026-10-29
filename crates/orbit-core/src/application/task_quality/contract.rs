//! Consumer-owned expansion of the offline evaluator's frozen v1 predicates.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::assistance::{
    self, AssistanceContract, AssistanceCriterionRef, AssistanceEvidencePacket, AssistanceQuestion,
    AssistanceRequest,
};
use orbit_types::task::Task;
use serde_json::json;

pub const TASK_QUALITY_USE_CASE: &str = "task_quality";

/// The exact template granted by the host. No confidence threshold is enabled
/// before the independently labeled evaluation; all valid answers are uncertain.
pub fn task_quality_question_template() -> AssistanceContract {
    let questions = [
        ("outcome_unclear", "True only if the whole task names no observable outcome. Valid investigations delivering evidence are not unclear."),
        ("criterion_not_observable", "True per criterion that names no observable outcome; cite criterion_refs."),
        ("requirements_conflict", "True only for criterion pairs that cannot both hold; cite both refs."),
        ("scope_clarification", "True only if unrelated deliverables lack a common outcome. Coherent cross-component work is not flagged."),
    ].into_iter().map(|(name, rubric)| (name.into(), AssistanceQuestion::Predicate {
        rubric: rubric.into(), min_confidence: None, criterion_refs: Vec::new(),
    })).collect();
    AssistanceContract {
        format_version: assistance::ASSISTANCE_FORMAT.into(),
        question_set_version: "task-quality-questions/1".into(),
        rubric_version: "task-quality-rubric/1".into(),
        evidence_builder_version: "task-quality-evidence/1".into(),
        result_schema_version: assistance::ASSISTANCE_RESULT_SCHEMA.into(),
        questions,
    }
}

pub(super) fn task_quality_digest(task: &Task) -> Result<String, OrbitError> {
    // Ordered criteria bind the numbered references. Reordering must not reuse
    // an annotation pointing at a different criterion at the same index.
    Ok(assistance::assistance_digest(
        &serde_json::to_vec(&(&task.title, &task.description, &task.acceptance_criteria))
            .map_err(super::ledger::quality_json_error)?,
    ))
}

pub(super) fn task_quality_request(
    task: &Task,
    workspace: &str,
    collected_at: DateTime<Utc>,
) -> Result<AssistanceRequest, OrbitError> {
    // 2 whole-task + 27 criterion + 351 pair questions fit the service's
    // 400-question bound. Never silently omit criteria and report clean.
    if task.acceptance_criteria.len() > 27 {
        return Err(OrbitError::InvalidInput(
            "task quality exceeds its bounded criterion contract".into(),
        ));
    }
    let digest = task_quality_digest(task)?;
    let template = task_quality_question_template();
    let mut contract = template.clone();
    contract.questions.clear();
    let references: Vec<_> = task
        .acceptance_criteria
        .iter()
        .enumerate()
        .map(|(i, text)| AssistanceCriterionRef {
            index: i as u32 + 1,
            text_digest: assistance::assistance_digest(text.as_bytes()),
        })
        .collect();
    let mut add = |name: String, base: &str, refs: Vec<AssistanceCriterionRef>| {
        let AssistanceQuestion::Predicate {
            rubric,
            min_confidence,
            ..
        } = &template.questions[base]
        else {
            unreachable!()
        };
        contract.questions.insert(
            name,
            AssistanceQuestion::Predicate {
                rubric: rubric.clone(),
                min_confidence: *min_confidence,
                criterion_refs: refs,
            },
        );
    };
    for rule in ["outcome_unclear", "scope_clarification"] {
        add(rule.into(), rule, Vec::new());
    }
    for reference in &references {
        add(
            format!("criterion_not_observable/{}", reference.index),
            "criterion_not_observable",
            vec![reference.clone()],
        );
    }
    for (i, first) in references.iter().enumerate() {
        for second in &references[i + 1..] {
            add(
                format!("requirements_conflict/{}/{}", first.index, second.index),
                "requirements_conflict",
                vec![first.clone(), second.clone()],
            );
        }
    }
    Ok(AssistanceRequest {
        workspace_id: workspace.into(),
        owner_id: workspace.into(),
        use_case: TASK_QUALITY_USE_CASE.into(),
        source_digest: digest.clone(),
        contract,
        evidence_packet: AssistanceEvidencePacket {
            packet_id: digest,
            subject_id: task.id.clone(),
            collected_at,
            evidence_refs: vec![format!("task:{}", task.id)],
            omissions: Vec::new(),
            truncated: false,
            fields: BTreeMap::from([
                ("title".into(), json!(task.title)),
                ("description".into(), json!(task.description)),
                (
                    "criteria".into(),
                    json!(
                        references
                            .iter()
                            .zip(&task.acceptance_criteria)
                            .map(|(r, text)| json!({
                                "index": r.index, "text_digest": r.text_digest, "text": text,
                            }))
                            .collect::<Vec<_>>()
                    ),
                ),
                ("criterion_refs".into(), json!(references)),
            ]),
        },
    })
}
