use crate::{GlobalSearchKind, GlobalSearchParams, OrbitRuntime, WorkspaceScope};
use orbit_common::protocol::tool_input::{optional_string_list_alias, optional_u32_alias};
use orbit_common::{
    OrbitError,
    governance::{operation::OperationVerb, search::SearchVerb},
};
use serde_json::Value;
use std::str::FromStr;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: SearchVerb,
    mut input: Value,
) -> Result<Value, OrbitError> {
    // Direct host calls use the same canonical shape as the generic tool entry.
    orbit_common::governance::search::normalize_legacy_input(&mut input);
    let input = verb.spec().parse_input(input)?;
    match verb {
        SearchVerb::Query => {
            let kind = input
                .optional_string("kind")?
                .map(|kind| GlobalSearchKind::from_str(&kind).map_err(OrbitError::InvalidInput))
                .transpose()?
                .unwrap_or_default();
            let workspaces = WorkspaceScope::from_inputs(
                input
                    .optional_string_list("workspaces")?
                    .unwrap_or_default(),
                input.optional_bool("all_workspaces")?.unwrap_or(false),
            );
            let limit = optional_u32_alias(input.as_value(), &["limit"])?
                .map(|limit| limit as usize)
                .unwrap_or(0);
            let result = runtime.global_search(GlobalSearchParams {
                query: input.optional_string("query")?,
                kind,
                limit,
                tags: optional_string_list_alias(input.as_value(), &["tag"])?.unwrap_or_default(),
                all: input.optional_bool("all")?.unwrap_or(false),
                status: input.optional_string_list("status")?.unwrap_or_default(),
                path: None,
                workspaces,
            })?;
            serde_json::to_value(result)
                .map_err(|error| OrbitError::Execution(format!("serialize search result: {error}")))
        }
    }
}
