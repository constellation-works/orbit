//! Dashboard polling and SSE replay over the exact same runtime read operation.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use futures_core::Stream;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::task::locks::lease;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};

use super::leases::{LeaseCaller, RuntimeLeaseBackend};
use super::{bad_request, map_runtime_error, server_error};
use crate::state::DashboardState;

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub(super) fn request_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

#[derive(Default, Deserialize)]
pub(super) struct LeaseNoticeQuery {
    #[serde(flatten)]
    caller: LeaseCaller,
    after: Option<u64>,
    lease_id: Option<String>,
    limit: Option<u32>,
}

impl LeaseNoticeQuery {
    fn input(&self) -> Value {
        let mut input = json!({"after": self.after.unwrap_or(0)});
        if let Some(id) = &self.lease_id {
            input["lease_id"] = json!(id);
        }
        if let Some(limit) = self.limit {
            input["limit"] = json!(limit);
        }
        input
    }
}

async fn notice_page(runtime: Arc<OrbitRuntime>, input: Value) -> Result<Value, Box<Response>> {
    match tokio::task::spawn_blocking(move || lease::notices(&runtime, input)).await {
        Ok(Ok(page)) => Ok(page),
        Ok(Err(error)) => Err(Box::new(map_runtime_error(error))),
        Err(error) => Err(Box::new(server_error(orbit_core::OrbitError::Execution(
            format!("lease notice read panicked: {error}"),
        )))),
    }
}

pub(super) async fn get_lease_notices(
    State(state): State<DashboardState>,
    Query(query): Query<LeaseNoticeQuery>,
) -> Response {
    let runtime = match (RuntimeLeaseBackend { state }).lease_runtime(&query.caller, false) {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    match notice_page(runtime, query.input()).await {
        Ok(page) => Json(page).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn stream_lease_notices(
    State(state): State<DashboardState>,
    Query(mut query): Query<LeaseNoticeQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(cursor) = headers.get("last-event-id") {
        query.after = match cursor.to_str().ok().and_then(|raw| raw.parse::<u64>().ok()) {
            Some(after) => Some(after),
            None => {
                return bad_request("Last-Event-ID must be a non-negative sequence number".into());
            }
        };
    }
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let permit = match GATE
        .get_or_init(|| Arc::new(Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
    {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "lease stream limit reached; retry shortly",
            )
                .into_response();
        }
    };
    let runtime = match (RuntimeLeaseBackend { state }).lease_runtime(&query.caller, false) {
        Ok(runtime) => runtime,
        Err(response) => return *response,
    };
    let mut input = query.input();
    let first = match notice_page(runtime.clone(), input.clone()).await {
        Ok(page) => page,
        Err(response) => return *response,
    };
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let _permit = permit;
        let mut page = first;
        loop {
            if tx.is_closed() || SHUTTING_DOWN.load(Ordering::Relaxed) {
                return;
            }
            for notice in page["notices"].as_array().into_iter().flatten() {
                let frame = format!("id: {}\ndata: {notice}\n\n", notice["seq"]);
                if !send_notice_frame(&tx, frame).await {
                    return;
                }
            }
            input["after"] = page["cursor"].clone();
            if page["notices"].as_array().is_none_or(Vec::is_empty) {
                tokio::select! {
                    () = tx.closed() => return,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
                if !send_notice_frame(&tx, ": keep-alive\n\n".into()).await {
                    return;
                }
            }
            page = match notice_page(runtime.clone(), input.clone()).await {
                Ok(page) => page,
                Err(_) => return, // Reconnect resumes from the last delivered id.
            };
        }
    });
    let mut response = Response::new(Body::from_stream(LeaseNoticeFrames { rx }));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

struct LeaseNoticeFrames {
    rx: mpsc::Receiver<String>,
}

// A slow client may fill the bounded queue. Check shutdown even while waiting
// for room, retaining this event until it is queued or the connection closes.
async fn send_notice_frame(tx: &mpsc::Sender<String>, frame: String) -> bool {
    loop {
        if SHUTTING_DOWN.load(Ordering::Relaxed) {
            return false;
        }
        tokio::select! {
            permit = tx.reserve() => {
                let Ok(permit) = permit else { return false; };
                permit.send(frame);
                return true;
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

impl Stream for LeaseNoticeFrames {
    type Item = Result<String, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|item| item.map(Ok))
    }
}
