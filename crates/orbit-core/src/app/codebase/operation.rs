//! The codebase registry flows: each calls the codebase function its tool
//! handler called before.

use orbit_common::OrbitError;
use orbit_common::governance::codebase::CodebaseVerb;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use super::{
    CodebaseQuery, RegressionFilter, RelatedQuery, scan_analytics, scan_path_detail, scan_related,
};
use crate::OrbitRuntime;

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
            scan_related(
                runtime,
                &selection.codebase_query(),
                &input.required_string("path")?,
                selection.top(),
            )
        }
        CodebaseVerb::Analytics => scan_analytics(runtime, &analytics_query(runtime, input)?),
        CodebaseVerb::Show => scan_path_detail(
            runtime,
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
