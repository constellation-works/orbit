//! Workspace change stream: `GET /api/events/stream?workspace=<id>`.
//!
//! One SSE connection carries small change notices for the selected
//! workspace, never full payloads, so the dashboard refetches only what a
//! notice names. Each notice is one unnamed SSE message whose JSON `kind` is a
//! workspace data kind:
//!
//! - `task` `{id, status, updated_at}`: a task was created or changed;
//!   `status` and `updated_at` are null once the task is no longer listed.
//! - `run` `{id, task_id, state}`: a run changed state.
//! - `locks` `{}`: the workspace's held locks may have changed.
//! - `drain` `{phase}`: on connect, and whenever the drain phase changes.
//! - `host.resources` `{resources}`: the serving host's `/host/resources`
//!   body, every few seconds.
//! - `resync` `{reason}`: changes may have been missed; refetch everything.
//!
//! The source is the store itself, scanned by one thread per connection, so a
//! write from any process (the CLI, a drain, MCP) arrives within one scan
//! interval. Every notice's `id` is the time of the scan that produced it, in
//! milliseconds. A reconnect's `Last-Event-ID` replays each task and run whose
//! change stamp falls after that time, less [`RESUME_SLACK_SECONDS`] for a
//! write stamped before it committed, and repeats `locks`; notices are
//! idempotent, so a repeat costs one refetch. An id the server cannot read or
//! a replay over [`MAX_NOTICES_PER_SCAN`] gets one `resync` notice instead.
//!
//! The connection cap and cooperative shutdown mirror `/api/log/stream`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Duration, TimeZone, Utc};
use futures_core::Stream;
use orbit_core::application::change_watch::RunStateStamp;
use orbit_core::{OrbitRuntime, TaskStatus};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::host::host_resources_json;
use super::server_error;
use crate::state::{DashboardState, Ws};

/// Most concurrent `/api/events/stream` clients. Each accepted stream pins one
/// native scanning thread, so this bounds threads and store reads the same way
/// `/api/log/stream`'s cap does.
const EVENTS_STREAM_MAX_CONCURRENT: usize = 8;
const EVENTS_STREAM_CHANNEL_DEPTH: usize = 64;
/// How often a stream compares the store with its previous scan.
const SCAN_INTERVAL: StdDuration = StdDuration::from_secs(2);
/// How often a stream sends the host resource sample.
const HOST_RESOURCES_INTERVAL: StdDuration = StdDuration::from_secs(5);
/// The drain phase is read after any run changes, and at least this often for
/// a window deadline that passes with no run changing.
const DRAIN_RECHECK_INTERVAL: StdDuration = StdDuration::from_secs(15);
/// Longest wait between disconnect and shutdown checks.
const SLEEP_SLICE: StdDuration = StdDuration::from_millis(250);
/// The browser's reconnect delay after a dropped stream.
const RECONNECT_DELAY_MS: u32 = 5_000;
/// A replay reaches this far before the resumed id, for a write stamped
/// before it committed.
const RESUME_SLACK_SECONDS: i64 = 60;
/// More notices than this from one scan or replay become one `resync`.
const MAX_NOTICES_PER_SCAN: usize = 500;
/// Each run read overlaps the previous one by this much, for a state change
/// stamped just before it committed.
const RUN_READ_OVERLAP_SECONDS: i64 = 10;

fn events_stream_gate() -> &'static Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Semaphore::new(EVENTS_STREAM_MAX_CONCURRENT)))
}

/// Set once a shutdown signal is received, so every open stream closes on its
/// next check instead of holding the graceful drain until its client leaves
/// (ORB-11246, as for `/api/log/stream`).
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Tell every open (and future) change stream to close now.
pub(super) fn request_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

pub(super) async fn stream_events(
    State(state): State<DashboardState>,
    Ws(runtime): Ws,
    headers: HeaderMap,
) -> Response {
    let Ok(permit) = Arc::clone(events_stream_gate()).try_acquire_owned() else {
        return events_stream_unavailable();
    };
    let resume = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(parse_event_id);
    let (tx, rx) = mpsc::channel(EVENTS_STREAM_CHANNEL_DEPTH);
    let spawned = thread::Builder::new()
        .name("orbit-web-events".into())
        .spawn(move || {
            // Released when the thread exits: within one sleep slice of the
            // client disconnecting or shutdown, or after a scan in progress.
            let _permit: OwnedSemaphorePermit = permit;
            stream_workspace_changes(&runtime, &state, resume, &tx);
        });
    if let Err(e) = spawned {
        return server_error(orbit_core::OrbitError::Execution(format!(
            "start change stream: {e}"
        )));
    }
    match Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(ReceiverSseStream { rx }))
    {
        Ok(response) => response,
        Err(e) => server_error(orbit_core::OrbitError::Execution(format!(
            "build SSE response: {e}"
        ))),
    }
}

fn events_stream_unavailable() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": format!(
                "change stream concurrency limit reached (max {EVENTS_STREAM_MAX_CONCURRENT}); retry shortly"
            )
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

/// A resumed id is a scan time in milliseconds; `None` when it is not one.
fn parse_event_id(raw: &str) -> Option<DateTime<Utc>> {
    let millis = raw.trim().parse::<i64>().ok()?;
    Utc.timestamp_millis_opt(millis).single()
}

fn event_id(at: DateTime<Utc>) -> String {
    at.timestamp_millis().to_string()
}

/// The stream thread: a first scan (and the replay a reconnect asks for), then
/// a scan every [`SCAN_INTERVAL`] and a host sample every
/// [`HOST_RESOURCES_INTERVAL`] until the client leaves or shutdown begins.
fn stream_workspace_changes(
    runtime: &OrbitRuntime,
    state: &DashboardState,
    resume: Option<Option<DateTime<Utc>>>,
    tx: &mpsc::Sender<String>,
) {
    if tx
        .blocking_send(format!("retry: {RECONNECT_DELAY_MS}\n\n"))
        .is_err()
    {
        return;
    }
    let mut watch = WorkspaceWatch::default();
    let mut notices = watch.scan(runtime);
    if let Some(resume) = resume {
        notices.extend(watch.replay(runtime, resume));
    }
    if !send_notices(tx, watch.scanned_at, notices) {
        return;
    }
    let mut next_host = Instant::now();
    let mut next_scan = Instant::now() + SCAN_INTERVAL;
    loop {
        if tx.is_closed() || SHUTTING_DOWN.load(Ordering::Relaxed) {
            return;
        }
        if Instant::now() >= next_host {
            let frame = match host_resources_json(state) {
                Ok(resources) => notice_frame(
                    watch.scanned_at,
                    &json!({"kind": "host.resources", "resources": resources}),
                ),
                // A comment keeps the connection's idle timers fed.
                Err(_) => ": host resources unavailable\n\n".to_owned(),
            };
            if tx.blocking_send(frame).is_err() {
                return;
            }
            next_host = Instant::now() + HOST_RESOURCES_INTERVAL;
        }
        if Instant::now() >= next_scan {
            let notices = watch.scan(runtime);
            if !send_notices(tx, watch.scanned_at, notices) {
                return;
            }
            next_scan = Instant::now() + SCAN_INTERVAL;
        }
        thread::sleep(SLEEP_SLICE);
    }
}

fn send_notices(tx: &mpsc::Sender<String>, at: DateTime<Utc>, notices: Vec<Value>) -> bool {
    let notices = if notices.len() > MAX_NOTICES_PER_SCAN {
        vec![resync_notice("too_many_changes")]
    } else {
        notices
    };
    notices
        .iter()
        .all(|notice| tx.blocking_send(notice_frame(at, notice)).is_ok())
}

fn notice_frame(at: DateTime<Utc>, notice: &Value) -> String {
    format!("id: {}\ndata: {notice}\n\n", event_id(at))
}

fn resync_notice(reason: &str) -> Value {
    json!({"kind": "resync", "reason": reason})
}

fn task_notice(id: &str, status: Option<TaskStatus>, updated_at: Option<DateTime<Utc>>) -> Value {
    json!({
        "kind": "task",
        "id": id,
        "status": status.map(|status| status.to_string()),
        "updated_at": updated_at.map(|at| at.to_rfc3339()),
    })
}

fn run_notice(stamp: &RunStateStamp) -> Value {
    json!({
        "kind": "run",
        "id": stamp.run_id,
        "task_id": stamp.task_id,
        "state": stamp.state.to_string(),
    })
}

/// A task in one of these statuses can hold or wait on locks, so its change
/// may change the workspace's locks.
fn holds_locks(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Backlog | TaskStatus::InProgress | TaskStatus::Review
    )
}

type TaskStamp = (TaskStatus, DateTime<Utc>);

/// What the previous scan read. A source whose first read has not succeeded
/// yet (`None`) produces no notices; its next successful read is its baseline.
struct WorkspaceWatch {
    tasks: Option<BTreeMap<String, TaskStamp>>,
    runs: Option<BTreeMap<String, RunStateStamp>>,
    run_read_at: DateTime<Utc>,
    reservations: Option<Vec<String>>,
    drain_phase: Option<&'static str>,
    drain_read_at: Option<Instant>,
    scanned_at: DateTime<Utc>,
}

impl Default for WorkspaceWatch {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            tasks: None,
            runs: None,
            run_read_at: now,
            reservations: None,
            drain_phase: None,
            drain_read_at: None,
            scanned_at: now,
        }
    }
}

impl WorkspaceWatch {
    /// Compare the store with the previous scan and name what changed. A read
    /// that fails keeps its previous baseline, so its changes surface on the
    /// next scan that reads it.
    fn scan(&mut self, runtime: &OrbitRuntime) -> Vec<Value> {
        let started = Utc::now();
        let mut notices = Vec::new();
        let mut locks = false;

        match runtime.task_change_stamps() {
            Ok(stamps) => {
                let next = stamps
                    .into_iter()
                    .map(|stamp| (stamp.id, (stamp.status, stamp.updated_at)))
                    .collect::<BTreeMap<_, _>>();
                if let Some(previous) = &self.tasks {
                    for (id, stamp) in &next {
                        let before = previous.get(id);
                        if before == Some(stamp) {
                            continue;
                        }
                        locks |= holds_locks(stamp.0) || before.is_some_and(|b| holds_locks(b.0));
                        notices.push(task_notice(id, Some(stamp.0), Some(stamp.1)));
                    }
                    for (id, before) in previous {
                        if !next.contains_key(id) {
                            locks |= holds_locks(before.0);
                            notices.push(task_notice(id, None, None));
                        }
                    }
                }
                self.tasks = Some(next);
            }
            Err(error) => tracing::debug!(%error, "change stream could not read task stamps"),
        }

        let mut runs_changed = false;
        let since = self.run_read_at - Duration::seconds(RUN_READ_OVERLAP_SECONDS);
        match runtime.run_state_stamps(since) {
            Ok(stamps) => {
                let mut next = stamps
                    .into_iter()
                    .map(|stamp| (stamp.run_id.clone(), stamp))
                    .collect::<BTreeMap<_, _>>();
                if let Some(previous) = &self.runs {
                    // A run that left the active set without a fresh activity
                    // stamp is outside both reads; ask for it directly.
                    for (id, before) in previous {
                        if next.contains_key(id) || before.state.is_terminal() {
                            continue;
                        }
                        if let Ok(Some(stamp)) = runtime.run_state_stamp(id) {
                            next.insert(id.clone(), stamp);
                        }
                    }
                    for (id, stamp) in &next {
                        if previous
                            .get(id)
                            .is_none_or(|before| before.state != stamp.state)
                        {
                            runs_changed = true;
                            notices.push(run_notice(stamp));
                        }
                    }
                }
                self.runs = Some(next);
                self.run_read_at = started;
            }
            Err(error) => tracing::debug!(%error, "change stream could not read run states"),
        }

        match runtime.active_task_reservation_keys() {
            Ok(keys) => {
                locks |= self
                    .reservations
                    .as_ref()
                    .is_some_and(|previous| *previous != keys);
                self.reservations = Some(keys);
            }
            Err(error) => tracing::debug!(%error, "change stream could not read reservations"),
        }
        if locks {
            notices.push(json!({"kind": "locks"}));
        }

        let drain_due = runs_changed
            || self
                .drain_read_at
                .is_none_or(|at| at.elapsed() >= DRAIN_RECHECK_INTERVAL);
        if drain_due {
            match runtime.workspace_drain_phase() {
                Ok(phase) => {
                    if self.drain_phase != Some(phase) {
                        notices.push(json!({"kind": "drain", "phase": phase}));
                    }
                    self.drain_phase = Some(phase);
                    self.drain_read_at = Some(Instant::now());
                }
                Err(error) => {
                    tracing::debug!(%error, "change stream could not read the drain phase")
                }
            }
        }

        self.scanned_at = started;
        notices
    }

    /// Notices a reconnect missed since `resume`, read after the first scan:
    /// each task and run stamped since then, and `locks`. An unreadable or
    /// future id, a failed read, or more than [`MAX_NOTICES_PER_SCAN`] notices
    /// is one `resync`.
    fn replay(&self, runtime: &OrbitRuntime, resume: Option<DateTime<Utc>>) -> Vec<Value> {
        let Some(resume) = resume.filter(|at| *at <= self.scanned_at) else {
            return vec![resync_notice("unknown_event_id")];
        };
        let since = resume - Duration::seconds(RESUME_SLACK_SECONDS);
        let Some(tasks) = &self.tasks else {
            return vec![resync_notice("unavailable")];
        };
        let Ok(runs) = runtime.run_state_stamps(since) else {
            return vec![resync_notice("unavailable")];
        };
        let mut notices = tasks
            .iter()
            .filter(|(_, (_, updated_at))| *updated_at >= since)
            .map(|(id, (status, updated_at))| task_notice(id, Some(*status), Some(*updated_at)))
            .collect::<Vec<_>>();
        notices.extend(runs.iter().map(run_notice));
        notices.push(json!({"kind": "locks"}));
        if notices.len() > MAX_NOTICES_PER_SCAN {
            return vec![resync_notice("too_many_changes")];
        }
        notices
    }
}

struct ReceiverSseStream {
    rx: mpsc::Receiver<String>,
}

impl Stream for ReceiverSseStream {
    type Item = Result<String, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|item| item.map(Ok))
    }
}
