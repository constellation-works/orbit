//! Executable-generation lifecycle, resumed state and service orchestration.

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use orbit_common::OrbitError;
use orbit_common::fs::generation;
use rmcp::ServiceExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::adapter::OrbitToolServer;

#[cfg(unix)]
use super::unix;
use super::{LIFECYCLE_INTERVAL_ENV, RESUME_ENV, Resumed, StdioExit, input, tracking, writer};

/// How often an idle session checks for a replacement or a pending switch.
const LIFECYCLE_INTERVAL: Duration = Duration::from_secs(2);

fn lifecycle_interval_from_env(value: Option<&str>, debug_build: bool) -> Duration {
    if debug_build
        && let Some(milliseconds) = value.and_then(|raw| raw.parse::<u64>().ok())
        && (20..=2_000).contains(&milliseconds)
    {
        return Duration::from_millis(milliseconds);
    }
    LIFECYCLE_INTERVAL
}

fn lifecycle_interval() -> Duration {
    lifecycle_interval_from_env(
        std::env::var(LIFECYCLE_INTERVAL_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// The session state one image hands the next.
#[derive(Serialize, Deserialize)]
struct ResumeState {
    /// The process that handed over. `exec` keeps the pid, so a child that
    /// merely inherited the environment can never mistake itself for the
    /// resumed server.
    pid: u32,
    /// The client's `initialize` params as sent, if it initialized.
    initialize: Option<Value>,
    /// Bytes of a partial line read before the handover, base64-encoded.
    carryover: String,
}

/// Read the handed-over session, when this image was exec'd to resume one.
pub(crate) fn resumed_session() -> Option<Resumed> {
    let raw = std::env::var(RESUME_ENV).ok()?;
    let state = match serde_json::from_str::<ResumeState>(&raw) {
        Ok(state) if state.pid == std::process::id() => state,
        Ok(_) => return None,
        Err(error) => {
            tracing::warn!(target: "orbit.mcp", "ignoring an unreadable resumed session: {error}");
            return None;
        }
    };
    let carryover = base64::engine::general_purpose::STANDARD
        .decode(state.carryover)
        .ok()?;
    let initialize = match state.initialize.map(serde_json::from_value) {
        Some(Ok(request)) => Some(request),
        Some(Err(error)) => {
            tracing::warn!(target: "orbit.mcp", "ignoring an unreadable resumed initialize: {error}");
            return None;
        }
        None => None,
    };
    Some(Resumed {
        initialize,
        carryover,
    })
}

/// What an idle session should do next.
enum Lifecycle {
    Continue,
    Yield,
    HandOver(PathBuf),
}

fn lifecycle() -> Lifecycle {
    if generation::process_participation().is_none() {
        return Lifecycle::Continue;
    }
    if let Some(executable) = generation::handover_target(Some(generation::RESUME_MCP_STDIO)) {
        return Lifecycle::HandOver(executable);
    }
    if generation::pending_switch_for_this_process().is_some() {
        return Lifecycle::Yield;
    }
    Lifecycle::Continue
}

/// Serve `server` over stdio until the client leaves or the session is
/// handed over. `resumed` replays a session handed over by a previous image.
pub(crate) async fn serve(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
) -> Result<StdioExit, OrbitError> {
    #[cfg(unix)]
    if let Some(stdin) = unix::NonBlockingStdin::new() {
        return serve_handing_over(server, resumed, stdin).await;
    }
    serve_plain(server, resumed).await
}

/// Whether a stdio session in this process could hand over: `serve` does
/// only when stdin can be polled. A process registers this before it serves,
/// so an updater knows which sessions follow a renamed candidate.
pub fn stdin_supports_handover() -> bool {
    #[cfg(unix)]
    {
        unix::stdin_is_pollable()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Without a pollable stdin (a regular file, `/dev/null`) the session is
/// never handed over. It still reads stdin line by line itself, so a line
/// rmcp would reject is answered before rmcp can consume and drop it.
async fn serve_plain(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
) -> Result<StdioExit, OrbitError> {
    use tokio::io::AsyncReadExt as _;
    let (carryover, initialize) = match resumed {
        Some(resumed) => (resumed.carryover, resumed.initialize),
        None => (Vec::new(), None),
    };
    let undelivered = Arc::new(AtomicUsize::new(0));
    let (lines, receiver) = mpsc::channel::<Vec<u8>>(64);
    let stdin = std::io::Cursor::new(carryover).chain(tokio::io::stdin());
    let pump = tokio::spawn(input::pump_plain_lines(
        stdin,
        lines,
        Arc::clone(&undelivered),
    ));
    let reader = input::LineReader {
        lines: receiver,
        current: Vec::new(),
        position: 0,
        undelivered,
    };
    let transport = (reader, writer::CompleteFrameStdout::new());
    let running = match initialize {
        Some(request) => {
            let peer_info = match server.resumed_peer_info(request) {
                Ok(peer_info) => peer_info,
                Err(error) => {
                    pump.abort();
                    let _ = pump.await;
                    return Err(start_error(&error));
                }
            };
            rmcp::service::serve_directly(server, transport, Some(peer_info))
        }
        None => match server.serve(transport).await {
            Ok(running) => running,
            Err(error) => {
                pump.abort();
                let _ = pump.await;
                return Err(start_error(&error));
            }
        },
    };
    let result = running
        .waiting()
        .await
        .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")));
    pump.abort();
    let _ = pump.await;
    result?;
    Ok(StdioExit::Closed)
}

fn start_error(error: &dyn std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("mcp serve_stdio start: {error}"))
}

#[cfg(unix)]
async fn serve_handing_over(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
    stdin: unix::NonBlockingStdin,
) -> Result<StdioExit, OrbitError> {
    let tracker = Arc::new(Mutex::new(tracking::Tracker::default()));
    let undelivered = Arc::new(AtomicUsize::new(0));
    let (lines, receiver) = mpsc::channel::<Vec<u8>>(64);
    let (stop, stopped) = mpsc::channel(1);
    let (carryover, initialize) = match resumed {
        Some(resumed) => (resumed.carryover, resumed.initialize),
        None => (Vec::new(), None),
    };
    if let Some(request) = &initialize {
        let mut tracker = tracking::lock_stdio_tracker(&tracker);
        tracker.initialize = serde_json::to_value(request).ok();
    }
    let pump = tokio::spawn(unix::pump_stdio_lines(
        stdin,
        carryover,
        lines,
        Arc::clone(&tracker),
        Arc::clone(&undelivered),
        stopped,
    ));
    let reader = input::LineReader {
        lines: receiver,
        current: Vec::new(),
        position: 0,
        undelivered: Arc::clone(&undelivered),
    };
    let writer = writer::TrackingWriter {
        inner: writer::CompleteFrameStdout::new(),
        line: Vec::new(),
        answered: Vec::new(),
        tracker: Arc::clone(&tracker),
    };
    // Waiting for the client's first initialize request is idle too. Keep
    // startup inside the cancellable service task so the lifecycle below can
    // refresh a quiet session before any client traffic arrives.
    let (cancel, mut cancelled) = tokio::sync::oneshot::channel::<()>();
    let mut service = tokio::spawn(async move {
        let start = async move {
            match initialize {
                Some(request) => {
                    let peer_info = server
                        .resumed_peer_info(request)
                        .map_err(|error| start_error(&error))?;
                    tracing::info!(target: "orbit.mcp", "resumed the stdio session after an executable handover");
                    Ok(rmcp::service::serve_directly(
                        server,
                        (reader, writer),
                        Some(peer_info),
                    ))
                }
                None => server
                    .serve((reader, writer))
                    .await
                    .map_err(|error| start_error(&error)),
            }
        };
        let running = tokio::select! {
            started = start => started?,
            _ = &mut cancelled => return Ok(()),
        };
        let service_cancel = running.cancellation_token();
        let waiting = running.waiting();
        tokio::pin!(waiting);
        let finished = tokio::select! {
            finished = &mut waiting => finished,
            _ = &mut cancelled => {
                service_cancel.cancel();
                waiting.await
            }
        };
        finished
            .map(|_| ())
            .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))
    });
    let mut interval = tokio::time::interval(lifecycle_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            finished = &mut service => {
                finished
                    .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))??;
                let _ = stop.send(unix::PumpStop::Always).await;
                let _ = pump.await;
                return Ok(StdioExit::Closed);
            }
            _ = interval.tick() => {}
        }
        if !tracking::stdio_session_is_idle(&tracker, &undelivered) {
            continue;
        }
        let decision = tokio::task::spawn_blocking(lifecycle)
            .await
            .unwrap_or(Lifecycle::Continue);
        if matches!(decision, Lifecycle::Continue)
            || !tracking::stdio_session_is_idle(&tracker, &undelivered)
        {
            continue;
        }
        // Let the pump check its own buffer before stopping: checking a
        // shared length here would race with another stdin read. A deferred
        // handover leaves both the reader and the service running.
        if matches!(decision, Lifecycle::HandOver(_)) {
            let (accepted, response) = tokio::sync::oneshot::channel();
            let _ = stop.send(unix::PumpStop::HandOver(accepted)).await;
            if matches!(response.await, Ok(false)) {
                tracing::debug!(
                    target: "orbit.mcp.handover",
                    handover_deferred = true,
                    "deferring executable handover until the partial request completes"
                );
                continue;
            }
        } else {
            let _ = stop.send(unix::PumpStop::Always).await;
        }
        // Wait out whatever was forwarded before the pump stopped: a
        // request answered, a notification consumed.
        let Ok(Ok(outcome)) = pump.await else {
            return Err(OrbitError::Execution(
                "mcp serve_stdio: the stdin reader stopped unexpectedly".into(),
            ));
        };
        // Hold the input open until the service is cancelled: on EOF the
        // service answers what it was forwarded and finishes, which would end
        // the session below instead of handing it over.
        let (carryover, _input) = match outcome {
            unix::PumpOutcome::Stopped { carryover, input } => (carryover, input),
            unix::PumpOutcome::Closed => {
                // The client left while this session was deciding.
                let _ = service.await;
                return Ok(StdioExit::Closed);
            }
        };
        while !tracking::stdio_session_is_idle(&tracker, &undelivered) {
            tokio::select! {
                _ = &mut service => return Ok(StdioExit::Closed),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        // Let the service finish handling a consumed notification.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = cancel.send(());
        let _ = service.await;
        unix::restore_blocking_stdin();
        return Ok(match decision {
            Lifecycle::HandOver(executable) => {
                let resume = serde_json::to_string(&ResumeState {
                    pid: std::process::id(),
                    initialize: tracking::lock_stdio_tracker(&tracker).initialize.clone(),
                    carryover: base64::engine::general_purpose::STANDARD.encode(&carryover),
                })
                .map_err(|error| {
                    OrbitError::Execution(format!("encode resumed session: {error}"))
                })?;
                tracing::info!(
                    target: "orbit.mcp",
                    executable = %executable.display(),
                    "handing the stdio session over to the installed executable"
                );
                StdioExit::HandOver { executable, resume }
            }
            _ => {
                tracing::info!(target: "orbit.mcp", "yielding to a pending Orbit upgrade");
                StdioExit::Yielded
            }
        });
    }
}
