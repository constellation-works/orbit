//! Experiment definitions Orbit ships, named by `orbit experiment create
//! --template`.

use orbit_common::OrbitError;
use serde_json::json;

use super::ExperimentDefinitionInput;

/// The `visual-review` template (visual-review design §12.1, §12.3):
/// `verify-mock-ui` deliveries without (control) and with (treatment) the
/// comparison reviewer, read on the post-landing comparison's mismatches.
fn visual_review_definition() -> ExperimentDefinitionInput {
    let arm = |name: &str, control: bool, reviewer: bool| {
        json!({
            "name": name,
            "control": control,
            "weight": 1,
            "overlay": { "config": { "values": { "visual_review.reviewer": reviewer } } },
        })
    };
    ExperimentDefinitionInput {
        name: None,
        hypothesis: Some(
            "Visual review: the mock comparison reviewer and its verifier, on top of the \
             implementer's self-check, capture and checks, leave fewer confirmed mock \
             mismatches on the landed head of verify-mock-ui tasks, at no more than 15% more \
             cost per task."
                .to_string(),
        ),
        arms: Some(json!([
            arm("control", true, false),
            arm("treatment", false, true)
        ])),
        population: Some(json!({
            "tags_include": ["verify-mock-ui"],
            "tags_exclude": ["no-diff-expected"],
        })),
        primary_metric: Some("visual_landed_mismatches".to_string()),
        guardrails: Some(json!({
            "cost_usd": "+15%",
            "lead_time": "+0.33",
            "delivery_done_rate": "-5pt",
        })),
        min_sample: Some(30),
        max_days: Some(60),
        mode: Some("live".to_string()),
    }
}

/// `input` over the shipped template `template`: every field `input`
/// gives replaces the template's.
pub(crate) fn experiment_definition_from_template(
    template: &str,
    input: ExperimentDefinitionInput,
) -> Result<ExperimentDefinitionInput, OrbitError> {
    let base = match template {
        "visual-review" => visual_review_definition(),
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "unknown experiment template `{other}`; expected visual-review"
            )));
        }
    };
    Ok(ExperimentDefinitionInput {
        name: input.name.or(base.name),
        hypothesis: input.hypothesis.or(base.hypothesis),
        arms: input.arms.or(base.arms),
        population: input.population.or(base.population),
        primary_metric: input.primary_metric.or(base.primary_metric),
        guardrails: input.guardrails.or(base.guardrails),
        min_sample: input.min_sample.or(base.min_sample),
        max_days: input.max_days.or(base.max_days),
        mode: input.mode.or(base.mode),
    })
}
