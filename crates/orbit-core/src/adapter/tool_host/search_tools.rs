use std::str::FromStr;

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{
    optional_csv_or_string_list_alias, optional_string_alias, optional_string_list_alias,
    optional_u32_alias,
};
use serde_json::Value;

use crate::{GlobalSearchKind, GlobalSearchParams, OrbitRuntime, WorkspaceScope};

use super::input::optional_bool_alias;

pub(super) fn search(runtime: &OrbitRuntime, input: Value) -> Result<Value, OrbitError> {
    for retired in [
        "related",
        "path",
        "hybrid",
        "semantic",
        "id",
        "task_id",
        "taskId",
        "field",
        "embedding_model",
        "embeddingModel",
        "embedding-model",
        "semantic_model",
        "semanticModel",
    ] {
        if input.get(retired).is_some() {
            return Err(OrbitError::InvalidInput(format!(
                "unknown parameter `{retired}`; search supports lexical queries only"
            )));
        }
    }
    let kind = optional_string_alias(&input, &["kind"])?
        .map(|kind| GlobalSearchKind::from_str(&kind).map_err(OrbitError::InvalidInput))
        .transpose()?
        .unwrap_or_default();

    // `workspaces` is the federated *scope*; `workspace` stays the reserved
    // routing selector that binds a call to one registered checkout, so the two
    // never collide [ORB-11027].
    let workspaces = WorkspaceScope::from_inputs(
        optional_csv_or_string_list_alias(&input, &["workspaces", "workspace_scope"])?
            .unwrap_or_default(),
        optional_bool_alias(&input, &["all_workspaces", "allWorkspaces"])?.unwrap_or(false),
    );

    let result = runtime.global_search(GlobalSearchParams {
        query: optional_string_alias(&input, &["query"])?,
        kind,
        limit: optional_u32_alias(&input, &["limit"])?
            .map(|limit| limit as usize)
            .unwrap_or(10),
        tags: optional_string_list_alias(&input, &["tag", "tags"])?.unwrap_or_default(),
        all: optional_bool_alias(&input, &["all"])?.unwrap_or(false),
        status: optional_csv_or_string_list_alias(&input, &["status", "statuses"])?
            .unwrap_or_default(),
        path: None,
        workspaces,
    })?;
    serde_json::to_value(result)
        .map_err(|error| OrbitError::Execution(format!("serialize search result: {error}")))
}
