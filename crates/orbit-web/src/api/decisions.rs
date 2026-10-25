//! Human decision requests: the workspace's open-request list and the
//! operator's answer action.
//!
//! Listing mirrors `orbit.decision.list`; answering goes through the same
//! core apply step as `orbit decision answer`
//! (`answer_decision_request_as_human`), so a dashboard click and a CLI
//! answer land identically — as the human writer, with the request then
//! reading `answered`. A stale answer is refused by core and changes
//! nothing.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Json, Response};
use orbit_core::OrbitError;
use orbit_types::task::DecisionGate;
use serde::Deserialize;
use serde_json::{Value, json};

use super::tasks::human_comment_author;
use super::{bad_request, blocking, non_empty_string, server_error};
use crate::state::Ws;

/// Query string for `GET /api/decisions`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct DecisionListQuery {
    /// Only requests waiting on this gate.
    #[serde(default)]
    gate: Option<String>,
    /// Only requests on this task.
    #[serde(default)]
    task_id: Option<String>,
    /// Include answered, superseded and expired requests.
    #[serde(default)]
    include_closed: bool,
}

/// Body of `POST /api/decisions/:id/answer`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct DecisionAnswerBody {
    /// The chosen answer id, one of the request's allowed answers.
    #[serde(default)]
    answer: Option<String>,
    /// Evidence or note kept with the answer.
    #[serde(default)]
    note: Option<String>,
    /// Requested human author label; sanitized like a comment author.
    #[serde(default)]
    author: Option<String>,
}

/// `GET /api/decisions` — decision requests across the workspace.
pub(super) async fn list_decisions(
    Ws(runtime): Ws,
    Query(query): Query<DecisionListQuery>,
) -> Response {
    let gate = match query.gate.as_deref() {
        None => None,
        Some(name) => match DecisionGate::from_name(name) {
            Some(gate) => Some(gate),
            None => {
                return bad_request(format!(
                    "unknown decision gate `{name}`; expected block_triage, pilot_admission, proposed_approval or review_approval"
                ));
            }
        },
    };
    let task_id = query.task_id;
    let include_closed = query.include_closed;
    match blocking("decision list", move || {
        let requests = runtime.list_decision_requests(gate, task_id.as_deref(), include_closed)?;
        let items = requests
            .iter()
            .map(|request| {
                serde_json::to_value(request).map_err(|error| {
                    OrbitError::Execution(format!("serialize decision request: {error}"))
                })
            })
            .collect::<Result<Vec<Value>, _>>()?;
        Ok(json!({ "items": items, "total": items.len() }))
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/decisions/:id/answer` — apply one allowed answer as the
/// human operator.
pub(super) async fn answer_decision(
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<DecisionAnswerBody>,
) -> Response {
    let Some(answer) = body.answer.as_deref().and_then(non_empty_string) else {
        return bad_request("answer must not be empty".to_string());
    };
    let note = body.note.as_deref().and_then(non_empty_string);
    let author = human_comment_author(body.author.as_deref());
    match blocking("decision answer", move || {
        runtime.answer_decision_request_as_human(
            &id,
            &answer,
            note,
            author,
            Some("dashboard".to_string()),
        )
    })
    .await
    {
        Ok(request) => match serde_json::to_value(&request) {
            Ok(value) => Json(json!({ "request": value })).into_response(),
            Err(error) => server_error(OrbitError::Execution(format!(
                "serialize decision request: {error}"
            ))),
        },
        Err(response) => *response,
    }
}
