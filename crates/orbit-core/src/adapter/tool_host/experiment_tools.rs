use crate::OrbitRuntime;
use crate::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput, ExperimentView,
};
use orbit_common::OrbitError;
use orbit_common::governance::experiment::ExperimentVerb;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use serde_json::{Value, json};

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: ExperimentVerb,
    input: Value,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let by = || -> Result<ExperimentActor, OrbitError> {
        Ok(ExperimentActor {
            actor: agent
                .or(model)
                .unwrap_or_else(|| runtime.actor_label())
                .to_string(),
            reason: input.optional_string("reason")?,
        })
    };
    let change = |change: ExperimentChange| -> Result<Value, OrbitError> {
        view_json(runtime.change_experiment(&input.required_string("id")?, change, by()?)?)
    };
    match verb {
        ExperimentVerb::Create => view_json(runtime.create_experiment(
            &input.required_string("name")?,
            definition_input(&input, None)?,
            by()?,
        )?),
        ExperimentVerb::Update => view_json(runtime.update_experiment(
            &input.required_string("id")?,
            definition_input(&input, input.optional_string("name")?)?,
            by()?,
        )?),
        ExperimentVerb::Clone => view_json(runtime.clone_experiment(
            &input.required_string("id")?,
            &input.required_string("name")?,
            by()?,
        )?),
        ExperimentVerb::Start => change(ExperimentChange::Start),
        ExperimentVerb::Pause => change(ExperimentChange::Pause),
        ExperimentVerb::Resume => change(ExperimentChange::Resume),
        ExperimentVerb::Reweight => change(ExperimentChange::Reweight {
            weights: input.as_value().get("weights").cloned(),
            retire: input.optional_string_list("retire")?.unwrap_or_default(),
        }),
        ExperimentVerb::Assign => Ok(json!({"experiments": runtime.force_experiment_assignment(
            &input.required_string("id")?, &input.required_string("task_id")?, &input.required_string("arm")?,
        )?})),
        ExperimentVerb::Conclude => change(ExperimentChange::Conclude {
            arm: input.optional_string("arm")?,
        }),
        ExperimentVerb::Abandon => change(ExperimentChange::Abandon),
        ExperimentVerb::Show => view_json(runtime.experiment_view(&input.required_string("id")?)?),
        ExperimentVerb::Readout => serde_json::to_value(runtime.experiment_readout(
            &input.required_string("id")?,
            input.optional_string("metric")?.as_deref(),
        )?)
        .map_err(|error| OrbitError::Execution(format!("serialize experiment readout: {error}"))),
        ExperimentVerb::List => {
            let experiments =
                runtime.experiment_views(input.optional_string("status")?.as_deref())?;
            Ok(json!({
                "count": experiments.len(),
                "experiments": experiments,
            }))
        }
    }
}

/// The definition fields create and update share; `name` is the rename an
/// update carries, never create's positional name.
fn definition_input(
    input: &OperationInput,
    name: Option<String>,
) -> Result<ExperimentDefinitionInput, OrbitError> {
    let field = |name: &str| input.as_value().get(name).cloned();
    Ok(ExperimentDefinitionInput {
        name,
        hypothesis: input.optional_raw_string("hypothesis")?,
        arms: field("arms"),
        population: field("population"),
        primary_metric: input.optional_string("primary_metric")?,
        guardrails: field("guardrails"),
        min_sample: input.optional_count("min_sample")?,
        max_days: input.optional_count("max_days")?,
        mode: input.optional_string("mode")?,
    })
}

fn view_json(view: ExperimentView) -> Result<Value, OrbitError> {
    serde_json::to_value(view)
        .map_err(|error| OrbitError::Execution(format!("serialize experiment: {error}")))
}
