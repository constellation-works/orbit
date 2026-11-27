//! Human decision requests: the workspace's open-request list and the
//! operator's answer action.
//!
//! Listing mirrors `orbit.decision` action `list`; answering goes through the same
//! core apply step as `orbit decision answer`
//! (`answer_decision_request_as_human`), so a dashboard click and a CLI
//! answer land identically — as the human writer, with the request then
//! reading `answered`. A stale answer is refused by core and changes
//! nothing.

use crate::state::run_read;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Json, Response};
use orbit_common::OrbitError;
use orbit_common::governance::authorization::{DASHBOARD_RESIDENT_WRITE, DASHBOARD_SCRIPT_GRANT};
use orbit_core::app::run::Command as RunCommand;
use orbit_core::app::task::{DashboardAction, Outcome};
use orbit_types::task::DecisionGate;
use serde::Deserialize;
use serde_json::json;

use super::routines::{authorization_denied, authorized_caller};
use super::tasks::human_comment_author;
use super::{bad_request, blocking, non_empty_string, server_error};
use crate::state::{DashboardState, Ws, WsCore};

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
    let gate = query.gate;
    let task_id = query.task_id;
    let include_closed = query.include_closed;
    match blocking("decision list", move || {
        run_read(
            &runtime,
            RunCommand::decision_requests(gate.as_deref(), task_id.as_deref(), include_closed),
        )
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// `POST /api/decisions/:id/answer` — apply one allowed answer as the
/// human operator. A `script_grant` answer writes a script grant and a
/// `resident_authority` answer widens a resident, so each takes the same
/// operator session as the Scripts or Residents view's own action.
pub(super) async fn answer_decision(
    State(state): State<DashboardState>,
    headers: axum::http::HeaderMap,
    Ws(runtime): Ws,
    Path(id): Path<String>,
    Json(body): Json<DecisionAnswerBody>,
) -> Response {
    let Some(answer) = body.answer.as_deref().and_then(non_empty_string) else {
        return bad_request("answer must not be empty".to_string());
    };
    let note = body.note.as_deref().and_then(non_empty_string);
    let author = human_comment_author(body.author.as_deref());
    let reader = std::sync::Arc::clone(&runtime);
    let lookup = id.clone();
    let (gate, existing_task_id) = match blocking("decision lookup", move || {
        Ok(run_read(&reader, RunCommand::decision_request(&lookup))?
            .map(|request| (Some(request.gate), Some(request.task_id)))
            .unwrap_or((None, None)))
    })
    .await
    {
        Ok(found) => found,
        Err(response) => return *response,
    };
    // An answer that writes authority takes the operator session of the
    // view that grants it: a script grant, or a resident's widening.
    let governed = match gate {
        Some(DecisionGate::ScriptGrant) => Some(&DASHBOARD_SCRIPT_GRANT),
        Some(DecisionGate::ResidentAuthority) => Some(&DASHBOARD_RESIDENT_WRITE),
        _ => None,
    };
    if let Some(governed) = governed
        && let Err(denial) = authorized_caller(governed, state.operator_session())
    {
        return authorization_denied(denial);
    }
    let mut meta = super::tasks::WebTaskMutationMeta::new(
        &state,
        &headers,
        "decision_answer",
        existing_task_id,
        json!({ "decision_id": id, "answer": answer }),
    );
    meta.role = Some(author.clone());
    let WsCore(core) = WsCore::over(&runtime, state.session());
    match blocking("decision answer", move || {
        super::tasks::run_audited_web_task_mutation(
            &core,
            meta,
            DashboardAction::AnswerDecision {
                id,
                answer,
                note,
                author,
            },
        )
        .and_then(Outcome::into_decision_request)
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
