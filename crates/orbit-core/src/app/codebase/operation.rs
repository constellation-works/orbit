//! The codebase registry flows: each calls the runtime method its tool
//! handler called before.

use orbit_common::OrbitError;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::codebase::{CodebaseQuery, RegressionFilter, RelatedQuery};

/// Run one codebase verb. The tool entry checked `input` against the verb's
/// spec.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: CodebaseVerb,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
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
        CodebaseVerb::Analytics => runtime.codebase_analytics(&analytics_query(runtime, input)?),
        CodebaseVerb::Show => runtime.codebase_path_detail(
            &analytics_query(runtime, input)?,
            &input.required_string("path")?,
        ),
    }
}

/// Window, minimum support, rollup and regression filters for analytics and
/// path detail; a layer is checked against the workspace's
/// `workflow.regression_layers`. Related does not use this: those fields are
/// not its parameters.
fn analytics_query(
    runtime: &OrbitRuntime,
    input: &OperationInput,
) -> Result<CodebaseQuery, OrbitError> {
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
        runtime.regression_layers(),
    )?))
}
