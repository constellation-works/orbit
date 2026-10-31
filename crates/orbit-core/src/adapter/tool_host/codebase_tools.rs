use crate::OrbitRuntime;
use crate::application::codebase::{CodebaseQuery, RegressionFilter, RelatedQuery};
use orbit_common::{
    OrbitError,
    governance::{
        codebase::CodebaseVerb,
        operation::{OperationInput, OperationVerb},
    },
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: CodebaseVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        CodebaseVerb::Related => {
            let selection = RelatedQuery::parse(
                input.optional_string("window")?.as_deref(),
                input.optional_count("min_support")?,
                input.optional_count("top")?,
            )?;
            runtime.codebase_related(
                &selection.codebase_query(),
                &input.required_string("path")?,
                selection.top(),
            )
        }
        CodebaseVerb::Analytics => runtime.codebase_analytics(&analytics_query(&input)?),
        CodebaseVerb::Show => {
            runtime.codebase_path_detail(&analytics_query(&input)?, &input.required_string("path")?)
        }
    }
}

/// Window, minimum support, rollup and regression filters for analytics and
/// path detail. Related does not use this: those fields are not its parameters.
fn analytics_query(input: &OperationInput) -> Result<CodebaseQuery, OrbitError> {
    Ok(CodebaseQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
        input.optional_string("rollup")?.as_deref(),
    )?
    .with_regression_filter(RegressionFilter::parse(
        input.optional_string("nature")?.as_deref(),
        input.optional_string("escape")?.as_deref(),
        input.optional_string("layer")?.as_deref(),
        input.optional_string("detected_by")?.as_deref(),
    )?))
}
