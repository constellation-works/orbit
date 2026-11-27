//! Friction artifact scan and triage handlers.
//!
//! ADR-0209 bearing 1 pilot [ORB-10358]. The verbs these routes call — their
//! tool names and their parameter names — come from the friction operation
//! registry rather than from string literals repeated per handler. What stays
//! hand-written here is genuinely HTTP: the route shapes, the JSON request
//! bodies, and the dashboard-specific defaults (`limit`, the human actor
//! fallback, the `tag_options` enrichment on GET).

use std::sync::Arc;

use crate::runtime_memo::FRICTION_STATS_TTL;
use crate::state::{DashboardState, Ws, WsCore, WsRejection};
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::governance::friction::{FRICTION_LIST_RESPONSE_MODE_WITH_NOTES, FrictionVerb};
use orbit_common::governance::operation::OperationVerb;
use orbit_core::app::friction::{self, Command, Input, Outcome};
use orbit_core::{Core, Error};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    OptionalJson, bad_request, blocking, bounded_limit, map_runtime_error, non_empty_string,
};

const FRICTIONS_DEFAULT_LIMIT: usize = 100;
const HUMAN_ACTOR_LABEL: &str = "human";

/// A friction tool call under construction.
///
/// Every field name is checked against the verb's registry spec as it is set,
/// so a dashboard/registry drift (a renamed or removed parameter) surfaces as a
/// 4xx naming the field instead of as a silently ignored filter.
struct FrictionCall {
    verb: FrictionVerb,
    input: Map<String, Value>,
}

impl FrictionCall {
    fn new(verb: FrictionVerb) -> Self {
        Self {
            verb,
            input: Map::new(),
        }
    }

    fn set(&mut self, param: &str, value: Value) -> Result<(), Error> {
        if !self
            .verb
            .spec()
            .params()
            .any(|declared| declared.name == param)
        {
            return Err(Error::Execution(format!(
                "{} does not declare a `{param}` parameter",
                self.verb.tool_name()
            )));
        }
        self.input.insert(param.to_string(), value);
        Ok(())
    }

    /// Pass on the request-body fields the verb's spec does not declare, so
    /// the tool entry reports them as it does for every other surface.
    /// Declared fields the route does not model stay out of the call.
    fn forward_undeclared(&mut self, extra: Map<String, Value>) {
        let spec = self.verb.spec();
        self.input.extend(
            extra
                .into_iter()
                .filter(|(field, _)| spec.is_undeclared_field(field)),
        );
    }

    /// Set a parameter only when the caller supplied a non-blank value.
    fn set_optional(&mut self, param: &str, value: Option<&str>) -> Result<(), Error> {
        match value.and_then(non_empty_string) {
            Some(value) => self.set(param, Value::String(value)),
            None => Ok(()),
        }
    }

    /// `orbit.friction` action `add` requires a non-empty `model`; when the caller
    /// supplies no attribution, default to the human actor label rather than a
    /// model constant. No other friction verb consumes `model`. A call that
    /// carries the human label is the operator's own, so it is attributed to
    /// them by the entry rather than by the field.
    fn into_command(mut self) -> Command {
        if self.verb == FrictionVerb::Add {
            self.input
                .entry("model".to_string())
                .or_insert_with(|| Value::String(HUMAN_ACTOR_LABEL.to_string()));
        }
        let input = if self.input.get("model").and_then(Value::as_str) == Some(HUMAN_ACTOR_LABEL) {
            Input::from_human(self.input)
        } else {
            Input::new(self.input)
        };
        Command::new(self.verb, input)
    }
}

#[derive(Deserialize, Default)]
pub(super) struct FrictionsQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    month: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

/// Create body for `POST /frictions`. Mirrors the `orbit.friction` action `add` tool
/// schema: `body` is required, `model` provenance and `tags`/`during_task`
/// are optional. Field parsing and validation stay in the shared runtime add
/// path so the wire shape and defaults match the native MCP tool exactly.
/// `model` falls back to the human actor label via [`FrictionCall::into_command`] when
/// the caller omits it. `orbit.friction` action `add` rejects
/// a separate `agent` field (attribution was consolidated to `model`-only),
/// so this body does not accept one. Fields the tool does not declare are
/// forwarded for the tool entry to report.
#[derive(Deserialize, Default)]
pub(super) struct CreateFrictionBody {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    during_task: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Deserialize, Default)]
pub(super) struct FrictionPatchBody {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Retitling a record is triage, not a body edit: an operator can give a
    /// legacy record a usable handle without touching its append-only report.
    #[serde(default)]
    title: Option<String>,
    /// Fields the tool does not declare, forwarded for the tool entry to
    /// report.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// A friction request's per-request [`Core`] and the shared runtime it was
/// derived from. The memo keys on the runtime, which every request of a
/// workspace shares; the per-request core is what runs the verbs.
pub(super) struct FrictionWs {
    core: Arc<Core>,
    runtime: Arc<Core>,
}

impl FrictionWs {
    /// The workspace's friction rates, computed once for the Open list, the
    /// Triaged list and the Stats read of one Frictions refresh. Keyed by the
    /// workspace runtime, so one workspace never serves another's rates, and a
    /// failed computation is not kept: the next read retries it.
    async fn stats(
        &self,
        state: &DashboardState,
    ) -> Result<crate::runtime_memo::Served<Value>, Box<Response>> {
        let core = Arc::clone(&self.core);
        state
            .friction_stats_memo()
            .get_or_compute(&self.runtime, (), FRICTION_STATS_TTL, move || {
                friction::exec(&core, FrictionCall::new(FrictionVerb::Stats).into_command())
                    .map(Outcome::into_value)
            })
            .await
            .map_err(|error| Box::new(map_runtime_error(error)))
    }

    /// Run a friction write and drop the workspace's memoized rates, so the
    /// next read reflects it instead of waiting out the memo.
    async fn write(&self, state: &DashboardState, call: FrictionCall) -> Response {
        let result = run_friction(Arc::clone(&self.core), call).await;
        state.friction_stats_memo().invalidate(&self.runtime);
        match result {
            Ok(friction) => Json(friction).into_response(),
            Err(response) => *response,
        }
    }
}

impl FromRequestParts<DashboardState> for FrictionWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        let WsCore(core) = WsCore::over(&runtime, state.session());
        Ok(Self { core, runtime })
    }
}

async fn run_friction(core: Arc<Core>, call: FrictionCall) -> Result<Value, Box<Response>> {
    blocking("friction tool", move || {
        friction::exec(&core, call.into_command()).map(Outcome::into_value)
    })
    .await
}

pub(super) async fn list_frictions(
    State(state): State<DashboardState>,
    ws: FrictionWs,
    Query(query): Query<FrictionsQuery>,
) -> Response {
    let mut call = FrictionCall::new(FrictionVerb::List);
    let built = (|| {
        call.set_optional("status", query.status.as_deref())?;
        call.set_optional("tag", query.tag.as_deref())?;
        call.set_optional("month", query.month.as_deref())?;
        call.set_optional("q", query.q.as_deref())?;
        call.set(
            "limit",
            Value::from(bounded_limit(query.limit, FRICTIONS_DEFAULT_LIMIT)),
        )?;
        call.set("offset", Value::from(query.offset.unwrap_or(0)))?;
        call.set(
            "response_mode",
            Value::String(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES.to_string()),
        )
    })();
    if let Err(e) = built {
        return map_runtime_error(e);
    }

    let list_core = Arc::clone(&ws.core);
    let (listed, stats) = tokio::join!(
        blocking("friction list", move || {
            let run = |call: FrictionCall| {
                friction::exec(&list_core, call.into_command()).map(Outcome::into_value)
            };
            let items = run(call)?;
            let tags = run(FrictionCall::new(FrictionVerb::Tags))?;
            Ok((items, tags))
        }),
        ws.stats(&state),
    );
    let (items_json, tags) = match listed {
        Ok(listed) => listed,
        Err(response) => return *response,
    };
    let stats = match stats {
        Ok(served) => (*served.body).clone(),
        Err(response) => return *response,
    };
    let (items, notes) = match items_json {
        Value::Array(items) => (items, Vec::new()),
        Value::Object(object) => {
            let items = object
                .get("records")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let notes = object
                .get("notes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            (items, notes)
        }
        other => {
            return map_runtime_error(Error::Execution(format!(
                "{} returned non-array JSON: {other}",
                FrictionVerb::List.tool_name()
            )));
        }
    };

    let mut body = json!({
        "stats": stats,
        "tags": tags,
        "items": items,
    });
    if !notes.is_empty()
        && let Some(object) = body.as_object_mut()
    {
        object.insert("notes".to_string(), Value::Array(notes));
    }
    Json(body).into_response()
}

/// `POST /frictions` — file a new friction, mirroring `orbit.friction` action `add`.
/// The request body is passed through the shared runtime add path so validation
/// and defaults stay identical to the native MCP tool; the created friction JSON
/// (same projection the list/get handlers emit) is returned on success.
/// Malformed JSON is rejected with a structured 400; deeper validation errors
/// (missing/empty `body`, bad fields) surface through [`map_runtime_error`] as
/// 4xx carrying Orbit's own error text.
pub(super) async fn create_friction_action(
    State(state): State<DashboardState>,
    ws: FrictionWs,
    body: Result<Json<CreateFrictionBody>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => {
            return bad_request(format!("malformed friction create payload: {rejection}"));
        }
    };

    let mut call = FrictionCall::new(FrictionVerb::Add);
    let built = (|| {
        call.set_optional("model", body.model.as_deref())?;
        call.set_optional("title", body.title.as_deref())?;
        if let Some(friction_body) = body.body {
            call.set("body", Value::String(friction_body))?;
        }
        if let Some(tags) = body.tags {
            call.set("tags", json!(tags))?;
        }
        call.set_optional("during_task", body.during_task.as_deref())?;
        call.forward_undeclared(body.extra);
        Ok(())
    })();
    if let Err(e) = built {
        return map_runtime_error(e);
    }

    ws.write(&state, call).await
}

pub(super) async fn get_friction(WsCore(core): WsCore, Path(id): Path<String>) -> Response {
    let call = match id_call(FrictionVerb::Show, id) {
        Ok(call) => call,
        Err(e) => return map_runtime_error(e),
    };
    let mut friction = match run_friction(core.clone(), call).await {
        Ok(friction) => friction,
        Err(response) => return *response,
    };
    let tags = match run_friction(core, FrictionCall::new(FrictionVerb::Tags)).await {
        Ok(tags) => tags,
        Err(response) => return *response,
    };
    if let Some(object) = friction.as_object_mut() {
        object.insert("tag_options".to_string(), tags);
    }
    Json(friction).into_response()
}

pub(super) async fn friction_stats(
    State(state): State<DashboardState>,
    ws: FrictionWs,
) -> Response {
    match ws.stats(&state).await {
        Ok(served) => (served.age_header(), Json((*served.body).clone())).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn update_friction_action(
    State(state): State<DashboardState>,
    ws: FrictionWs,
    Path(id): Path<String>,
    OptionalJson(body): OptionalJson<FrictionPatchBody>,
) -> Response {
    let status = body.status.as_deref().and_then(non_empty_string);
    let title = body.title.as_deref().and_then(non_empty_string);
    if status.is_none() && body.tags.is_none() && title.is_none() {
        return bad_request("request body must include `status`, `tags`, or `title`".to_string());
    }

    let built = id_call(FrictionVerb::Update, id).and_then(|mut call| {
        if let Some(status) = status {
            call.set("status", Value::String(status))?;
        }
        if let Some(tags) = body.tags {
            call.set("tags", json!(tags))?;
        }
        if let Some(title) = title {
            call.set("title", Value::String(title))?;
        }
        call.forward_undeclared(body.extra);
        Ok(call)
    });
    let call = match built {
        Ok(call) => call,
        Err(e) => return map_runtime_error(e),
    };

    ws.write(&state, call).await
}

pub(super) async fn resolve_friction_action(
    State(state): State<DashboardState>,
    ws: FrictionWs,
    Path(id): Path<String>,
) -> Response {
    let call = match id_call(FrictionVerb::Resolve, id) {
        Ok(call) => call,
        Err(e) => return map_runtime_error(e),
    };
    ws.write(&state, call).await
}

/// Seed a call with the path-supplied record id.
fn id_call(verb: FrictionVerb, id: String) -> Result<FrictionCall, Error> {
    let mut call = FrictionCall::new(verb);
    call.set("id", Value::String(id))?;
    Ok(call)
}
