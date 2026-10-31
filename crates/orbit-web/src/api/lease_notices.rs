//! Lease notice polling and SSE replay (design §5 "Push and poll"), served by
//! [`lease_router`](super::leases::lease_router) over any [`LeaseBackend`], so
//! Orbit Web and the lease listener frame the same pages the same way. The
//! page loop, [`follow_notice_pages`], runs in Orbit Web for both: for its own
//! streams, and on the intake socket for the listener's.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use orbit_core::runtime::task::locks::lease;
use orbit_core::{OrbitError, OrbitRuntime};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc};

use super::leases::{
    LeaseBackend, LeaseBackendResult, LeaseCaller, LeaseCallerParts, LeaseNoticePages,
    RuntimeLeaseBackend, lease_backend_call, lease_call_input,
};
use super::{bad_request, map_runtime_error, server_error};

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// How long a stream waits after an empty page before reading again.
const EMPTY_PAGE_WAIT: Duration = Duration::from_secs(1);

/// Orbit Web's own concurrent notice streams. The listener's streams are
/// bounded by its own limits instead.
const DASHBOARD_STREAMS: usize = 8;

pub(super) fn request_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

#[derive(Default, Deserialize)]
pub(super) struct LeaseNoticeQuery {
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

pub(super) async fn get_lease_notices(
    State(backend): State<Arc<dyn LeaseBackend>>,
    LeaseCallerParts(caller): LeaseCallerParts,
    Query(query): Query<LeaseNoticeQuery>,
) -> Response {
    lease_backend_call(backend, caller, query.input(), |b, c, i| b.notices(c, i)).await
}

pub(super) async fn stream_lease_notices(
    State(backend): State<Arc<dyn LeaseBackend>>,
    LeaseCallerParts(caller): LeaseCallerParts,
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
    let input = query.input();
    let caller = match lease_call_input(caller, &input) {
        Ok(caller) => caller,
        Err(response) => return *response,
    };
    let opened = tokio::task::spawn_blocking(move || backend.notice_stream(&caller, input)).await;
    let mut pages = match opened {
        Ok(Ok(pages)) => pages,
        Ok(Err(response)) => return *response,
        Err(error) => {
            return server_error(OrbitError::Execution(format!(
                "lease notice stream panicked: {error}"
            )));
        }
    };
    let first = match pages.recv().await {
        Some(Ok(page)) => page,
        Some(Err(response)) => return *response,
        None => {
            return server_error(OrbitError::Execution(
                "the lease notice stream closed before its first page".to_string(),
            ));
        }
    };
    let mut response = Response::new(Body::from_stream(LeaseNoticeFrames {
        first: Some(first),
        pages,
    }));
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

/// Orbit Web's stream: the first page read here, the rest by a task that
/// holds one of its [`DASHBOARD_STREAMS`] until the client goes.
pub(super) fn open_runtime_notice_stream(
    backend: &RuntimeLeaseBackend,
    caller: &LeaseCaller,
    input: Value,
) -> Result<LeaseNoticePages, Box<Response>> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let Ok(permit) = GATE
        .get_or_init(|| Arc::new(Semaphore::new(DASHBOARD_STREAMS)))
        .clone()
        .try_acquire_owned()
    else {
        return Err(Box::new(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "lease stream limit reached; retry shortly",
            )
                .into_response(),
        ));
    };
    let runtime = backend.lease_runtime(caller, false)?;
    let first =
        lease::notices(&runtime, input.clone()).map_err(|e| Box::new(map_runtime_error(e)))?;
    let (pages, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        let _permit = permit;
        follow_notice_pages(
            first,
            input,
            |input| read_runtime_page(Arc::clone(&runtime), input),
            pages,
        )
        .await;
    });
    Ok(receiver)
}

async fn read_runtime_page(runtime: Arc<OrbitRuntime>, input: Value) -> LeaseBackendResult {
    match tokio::task::spawn_blocking(move || lease::notices(&runtime, input)).await {
        Ok(Ok(page)) => Ok(page),
        Ok(Err(error)) => Err(Box::new(map_runtime_error(error))),
        Err(error) => Err(Box::new(server_error(OrbitError::Execution(format!(
            "lease notice read panicked: {error}"
        ))))),
    }
}

/// Send `first`, then each page read from the previous page's cursor,
/// waiting [`EMPTY_PAGE_WAIT`] after an empty one. Stops after sending a read
/// error, or when `pages` closes or the server shuts down. `input` carries
/// the stream's `lease_id` and `limit`; its `after` is replaced per page.
pub(crate) async fn follow_notice_pages<E, F, Fut>(
    first: Value,
    mut input: Value,
    mut read_page: F,
    pages: mpsc::Sender<Result<Value, E>>,
) where
    F: FnMut(Value) -> Fut,
    Fut: Future<Output = Result<Value, E>>,
{
    let mut page = first;
    loop {
        let empty = page["notices"].as_array().is_none_or(Vec::is_empty);
        input["after"] = page["cursor"].clone();
        if !send_page(&pages, Ok(page)).await {
            return;
        }
        if empty {
            tokio::select! {
                () = pages.closed() => return,
                () = tokio::time::sleep(EMPTY_PAGE_WAIT) => {},
            }
        }
        if SHUTTING_DOWN.load(Ordering::Relaxed) {
            return;
        }
        page = match read_page(input.clone()).await {
            Ok(page) => page,
            Err(error) => {
                send_page(&pages, Err(error)).await;
                return;
            }
        };
    }
}

// A slow client may fill the bounded queue. Check shutdown even while waiting
// for room, retaining this page until it is queued or the stream closes.
async fn send_page<T>(pages: &mpsc::Sender<T>, page: T) -> bool {
    loop {
        if SHUTTING_DOWN.load(Ordering::Relaxed) {
            return false;
        }
        tokio::select! {
            permit = pages.reserve() => {
                let Ok(permit) = permit else { return false; };
                permit.send(page);
                return true;
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {},
        }
    }
}

/// One SSE event per notice, its `seq` as the event id; a comment for an
/// empty page keeps the connection alive.
fn notice_page_frames(page: &Value) -> String {
    let notices = page["notices"].as_array().map_or(&[][..], Vec::as_slice);
    if notices.is_empty() {
        return ": keep-alive\n\n".to_string();
    }
    notices
        .iter()
        .map(|notice| format!("id: {}\ndata: {notice}\n\n", notice["seq"]))
        .collect()
}

struct LeaseNoticeFrames {
    first: Option<Value>,
    pages: LeaseNoticePages,
}

impl Stream for LeaseNoticeFrames {
    type Item = Result<String, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(page) = self.first.take() {
            return Poll::Ready(Some(Ok(notice_page_frames(&page))));
        }
        // An error mid-stream ends it; the client resumes from its last id.
        self.pages.poll_recv(cx).map(|page| match page {
            Some(Ok(page)) => Some(Ok(notice_page_frames(&page))),
            Some(Err(_)) | None => None,
        })
    }
}
