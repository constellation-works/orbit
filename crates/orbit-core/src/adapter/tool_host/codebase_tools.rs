use crate::OrbitRuntime;
use crate::application::codebase::CodebaseQuery;
use orbit_common::{
    OrbitError,
    governance::{codebase::CodebaseVerb, operation::OperationVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: CodebaseVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let query = CodebaseQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
        input.optional_string("rollup")?.as_deref(),
    )?;
    match verb {
        CodebaseVerb::Analytics => runtime.codebase_analytics(&query),
        CodebaseVerb::Show => runtime.codebase_path_detail(&query, &input.required_string("path")?),
    }
}
