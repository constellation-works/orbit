//! Knowledge › Memory: read-only crew memory for the dashboard.
//!
//! * `GET /api/memory/spaces` — every memory key with its current usage.
//! * `GET /api/memory/space?key=<key>` — one key's current revision and files.
//! * `GET /api/memory/history?key=<key>` — one key's revisions and events.
//! * `GET /api/memory/proposals` — open and recently closed global proposals.
//!
//! Each route calls the crew memory operation the `orbit memory` CLI uses.
//! None writes: the proposals read expires nothing (an operator's list or
//! decision does), and accepting, rejecting and every memory write stay on
//! the operator CLI.

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;

use super::{bad_request, blocking};
use crate::state::Ws;

/// `?key=<model id|global>` names the memory key.
#[derive(Debug, Default, Deserialize)]
pub(super) struct MemoryKeyQuery {
    #[serde(default)]
    key: Option<String>,
}

impl MemoryKeyQuery {
    fn required(self) -> Result<String, Box<Response>> {
        self.key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                Box::new(bad_request(
                    "`key` is required: a model id or `global`".to_string(),
                ))
            })
    }
}

pub(super) async fn list_memory_spaces(Ws(runtime): Ws) -> Response {
    json_or_error(blocking("list crew memory", move || runtime.crew_memory_spaces()).await)
}

pub(super) async fn show_memory_space(
    Ws(runtime): Ws,
    Query(query): Query<MemoryKeyQuery>,
) -> Response {
    let key = match query.required() {
        Ok(key) => key,
        Err(response) => return *response,
    };
    json_or_error(
        blocking("show crew memory", move || {
            runtime.crew_memory_show(Some(&key), None)
        })
        .await,
    )
}

pub(super) async fn memory_history(
    Ws(runtime): Ws,
    Query(query): Query<MemoryKeyQuery>,
) -> Response {
    let key = match query.required() {
        Ok(key) => key,
        Err(response) => return *response,
    };
    json_or_error(
        blocking("crew memory history", move || {
            runtime.crew_memory_history(&key)
        })
        .await,
    )
}

pub(super) async fn list_memory_proposals(Ws(runtime): Ws) -> Response {
    json_or_error(
        blocking("list crew memory proposals", move || {
            runtime.crew_memory_proposal_board()
        })
        .await,
    )
}

fn json_or_error(result: Result<serde_json::Value, Box<Response>>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}
