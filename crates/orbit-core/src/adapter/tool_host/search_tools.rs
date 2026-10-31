use crate::{GlobalSearchKind, OrbitRuntime, SearchRequest, SearchRequestProfile};
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
            let request = SearchRequest {
                query: input.optional_string("query")?,
                kind: input
                    .optional_string("kind")?
                    .map(|kind| GlobalSearchKind::from_str(&kind).map_err(OrbitError::InvalidInput))
                    .transpose()?
                    .unwrap_or_default(),
                limit: optional_u32_alias(input.as_value(), &["limit"])?
                    .map(|limit| limit as usize),
                tags: optional_string_list_alias(input.as_value(), &["tag"])?.unwrap_or_default(),
                all: input.optional_bool("all")?.unwrap_or(false),
                status: input.optional_string_list("status")?.unwrap_or_default(),
                // The shared spec rejects the retired `path` field, so the
                // tool profile never carries one.
                path: None,
                workspace_selectors: input
                    .optional_string_list("workspaces")?
                    .unwrap_or_default(),
                all_workspaces: input.optional_bool("all_workspaces")?.unwrap_or(false),
            };
            let result = runtime.search_with_profile(SearchRequestProfile::Tool, request)?;
            serde_json::to_value(result)
                .map_err(|error| OrbitError::Execution(format!("serialize search result: {error}")))
        }
    }
}
