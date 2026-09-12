//! `orbit-web` — HTTP API, dashboard UI, and remote web connection.
//!
//! This crate isolates the axum-based dashboard (HTML/JS assets + `/api/*`
//! handlers) from orbit-cli so that CLI changes do not force rebuilds of the
//! large dependency subtree (axum, etc). Behavior is identical to the prior
//! in-tree implementation.
//!
//! Public surface is deliberately tiny: `ServeArgs` (clap) plus two entry
//! points — `serve()` for a caller-supplied runtime (single-workspace mode)
//! and `serve_from_env()`, the entry point for `orbit web serve`, which
//! always serves every registered workspace (global mode; see ORB-10029).
//! All routes, content types, defaults, and graceful shutdown are preserved.

mod api;
mod connect;
mod health;
mod log_format;
mod parse;
mod projections;
mod ssh_tunnel;
mod state;

#[cfg(test)]
mod tests;

pub use connect::{ConnectArgs, connect};

use std::future::{Future, IntoFuture};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use clap::Args;
use orbit_cmd::registry_runtime;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{WorkspaceRegistry, WorkspaceStatus};
use tokio::sync::Notify;

const INDEX_HTML: &str = include_str!("../assets/dashboard/index.html");
const DASHBOARD_CSS: &str = include_str!("../assets/dashboard/dashboard.css");
const INTER_FONT: &[u8] = include_bytes!("../assets/dashboard/fonts/inter-latin.woff2");
const JETBRAINS_MONO_FONT: &[u8] =
    include_bytes!("../assets/dashboard/fonts/jetbrains-mono-latin.woff2");
const MARKED_JS: &str = include_str!("../assets/dashboard/marked.umd.js");
const PURIFY_JS: &str = include_str!("../assets/dashboard/purify.min.js");
// L-0021: Keep embedded dashboard JS modules in sync with /static routes.
const APP_JS: &str = include_str!("../assets/dashboard/app.js");
const COMMON_JS: &str = include_str!("../assets/dashboard/common.js");
const MARKDOWN_JS: &str = include_str!("../assets/dashboard/markdown.js");
const TASKS_JS: &str = include_str!("../assets/dashboard/tasks.js");
const FIELD_EDITOR_JS: &str = include_str!("../assets/dashboard/field-editor.js");
const AUDIT_JS: &str = include_str!("../assets/dashboard/audit.js");
const SCOREBOARD_JS: &str = include_str!("../assets/dashboard/scoreboard.js");
const RELIABILITY_JS: &str = include_str!("../assets/dashboard/reliability.js");
const LOG_TAIL_JS: &str = include_str!("../assets/dashboard/log-tail.js");
const DIAGNOSTICS_JS: &str = include_str!("../assets/dashboard/diagnostics.js");
const ROUTER_JS: &str = include_str!("../assets/dashboard/router.js");
const RUNS_JS: &str = include_str!("../assets/dashboard/runs.js");
const RUN_DETAIL_JS: &str = include_str!("../assets/dashboard/run-detail.js");
const AUTOMATION_JS: &str = include_str!("../assets/dashboard/automation.js");
const OPERATIONS_JS: &str = include_str!("../assets/dashboard/operations.js");
const DASHBOARD_CSP: &str = concat!(
    "default-src 'self'; ",
    "script-src 'self'; ",
    "style-src 'self' 'unsafe-inline'; ",
    "font-src 'self'; ",
    "img-src 'self' data:; ",
    "connect-src 'self'; ",
    "object-src 'none'; ",
    "base-uri 'none'; ",
    "frame-ancestors 'none'"
);

/// Conventional loopback port for the dashboard. Shared by `web serve`'s
/// `--port` default and `web connect`'s local/remote port preference so the
/// two surfaces agree on one number.
pub(crate) const DEFAULT_DASHBOARD_PORT: u16 = 7878;

/// Arguments for `orbit web serve` (and the library entry point).
#[derive(Args, Clone)]
#[command(about = "Run the Orbit dashboard")]
pub struct ServeArgs {
    /// Host or IP to bind to. Defaults to loopback for safety.
    #[arg(long, default_value = "127.0.0.1")]
    pub host: IpAddr,

    /// Port to bind to.
    #[arg(long, default_value_t = DEFAULT_DASHBOARD_PORT)]
    pub port: u16,

    /// Do not attempt to open the dashboard URL in a browser on startup.
    #[arg(long)]
    pub no_open: bool,

    // ORB-10029: source provenance for the global-only dashboard mode.
    /// Deprecated, no-op: `orbit web serve` always serves every registered
    /// workspace now (global mode is the only mode). Kept so
    /// the flag keeps parsing for existing scripts, and because `orbit web
    /// connect` unconditionally forwards it to the remote `orbit web serve`
    /// — removing it would break tunnels against an old/new binary mix.
    #[arg(long)]
    pub global: bool,

    /// Preselect this workspace in the dashboard, by registered name, logical
    /// ID (`ws_*`), or local checkout path. Defaults to the workspace
    /// containing the current directory. Distinct from `--root`, which
    /// chooses which registry is served.
    #[arg(long, value_name = "SELECTOR")]
    pub workspace: Option<String>,
}

/// Boot the dashboard for a single, already-built runtime and block until
/// shutdown (ctrl-c or SIGTERM). Single-workspace mode is no longer reachable
/// from `orbit web serve` (see [`serve_from_env`], ORB-10029); this stays for
/// callers that already hold an `OrbitRuntime` and want it embedded directly.
pub fn serve(runtime: &OrbitRuntime, args: ServeArgs) -> Result<(), OrbitError> {
    let state = state::DashboardState::single(Arc::new(runtime.clone()));
    run_server(&args, state)
}

/// Boot the dashboard, resolving every registered workspace from the current
/// environment, and block until shutdown.
///
/// Unlike [`serve`], this needs no pre-built runtime, so it works from any
/// directory — the entry point for `orbit web serve` (dispatched before the
/// CLI's eager workspace initialization, which would otherwise fail outside a
/// workspace). Always serves in global mode: every workspace registered in the
/// served registry is selectable via the dropdown, regardless of cwd
/// (`args.global` is accepted but ignored — see [`ServeArgs::global`]).
///
/// `root_override` is the top-level `--root <path>` flag, if given. It means
/// here exactly what it means everywhere else in the CLI: the Orbit data
/// directory to read, so the dashboard serves `<root>/workspaces.json` and
/// nothing from the machine-global registry (ORB-11388). Which workspace the
/// dropdown opens on is a separate question, answered by `--workspace`.
pub fn serve_from_env(args: ServeArgs, root_override: Option<&Path>) -> Result<(), OrbitError> {
    let state = build_state(root_override, args.workspace.as_deref())?;
    run_server(&args, state)
}

/// Resolve dashboard state from the environment: registry-backed global mode
/// over every workspace registered in the served registry (stale-path entries
/// are listed but marked inactive and never built). The registry is
/// `<root>/workspaces.json` for an explicit `--root`, and the machine-global
/// `~/.orbit/workspaces.json` otherwise — the same resolution every other
/// root-aware command performs, via
/// [`orbit_cmd::registry_runtime::global_root_for`]. The servable set is
/// reloaded from that same path on every request boundary (see
/// [`state::DashboardState::refresh`]), so a native `orbit workspace
/// init/remove` or binding change becomes visible without restarting the
/// server. The dropdown's default selection is, in priority order: the
/// registered/active workspace matching `workspace_selector` (an explicit
/// `--workspace`), else the registered workspace containing the cwd (see
/// [`default_workspace_for_cwd`]), else "All workspaces". See
/// [`default_workspace_selection`] for the precedence logic.
///
/// The initial load is eager: a malformed registry at startup is fatal, exactly
/// as before this became refreshable. A malformed *refresh* after a good
/// startup retains the last valid snapshot instead (see `refresh`).
fn build_state(
    root_override: Option<&Path>,
    workspace_selector: Option<&str>,
) -> Result<state::DashboardState, OrbitError> {
    let global_root = registry_runtime::global_root_for(root_override)?;
    let registry_path = workspace_registry::registry_path_for(&global_root);
    let cwd = std::env::current_dir().ok();
    let source = state::RegistrySource::new(
        registry_path,
        workspace_selector.map(ToOwned::to_owned),
        cwd,
    );
    state::DashboardState::from_registry(global_root, source)
}

/// Best-effort default when serving globally: the registered workspace whose
/// repo root is the longest prefix of `cwd`, if the server was launched inside
/// one. `None` means the frontend opens on the aggregate "all workspaces" view.
fn default_workspace_for_cwd(registry: &WorkspaceRegistry, cwd: &Path) -> Option<String> {
    workspace_registry::local_workspaces(registry)
        .filter(|(workspace, _)| workspace.status == WorkspaceStatus::Active)
        .filter_map(|(workspace, checkout)| {
            std::iter::once(&checkout.repo_root)
                .chain(&checkout.path_overrides)
                .filter(|candidate| cwd.starts_with(candidate))
                .map(|candidate| candidate.as_os_str().len())
                .max()
                .map(|prefix_len| (workspace, prefix_len))
        })
        .max_by_key(|(_, prefix_len)| *prefix_len)
        .map(|(workspace, _)| workspace.id.clone())
}

/// Precedence logic for the dropdown's default-preselected workspace: an
/// explicit `workspace_selector` (the `--workspace` flag) always wins over
/// `cwd` when given, even if it does not resolve to any registered/active
/// workspace — in that case the result is `None` ("All workspaces"), not a
/// fallback to the cwd-based default. This matches [`default_workspace_for_cwd`]:
/// don't error, don't auto-register, just prefer the aggregate view.
///
/// The selector is a registered name or logical `ws_*` ID first, and a local
/// checkout path when it is path-shaped
/// ([`registry_runtime::selector_looks_like_path`]); a path is matched the
/// same way a cwd is (longest registered prefix), which is what `orbit web
/// connect` forwards for a remote workspace directory. An unknown bare name is
/// never joined to cwd — that would preselect the cwd's workspace for a
/// selector that matched nothing. `workspace_selector` not being given falls
/// back to the existing cwd-based behavior unchanged.
fn default_workspace_selection(
    registry: &WorkspaceRegistry,
    workspace_selector: Option<&str>,
    cwd: Option<&Path>,
) -> Option<String> {
    match workspace_selector {
        Some(selector) => workspace_registry::resolve_logical_workspace(registry, selector)
            .ok()
            .filter(|workspace| workspace.status == WorkspaceStatus::Active)
            .map(|workspace| workspace.id.clone())
            .or_else(|| {
                if !registry_runtime::selector_looks_like_path(selector) {
                    return None;
                }
                let path = resolve_selector_path(Path::new(selector), cwd);
                default_workspace_for_cwd(registry, &path)
            }),
        None => cwd.and_then(|cwd| default_workspace_for_cwd(registry, cwd)),
    }
}

/// Normalize a path-shaped `--workspace <selector>` value so it can be
/// prefix-matched against registered workspace roots (which are canonical
/// absolute paths after the pipeline's canonicalization; see
/// `orbit-runtime/src/builder.rs`).
///
/// Relative paths are resolved against `cwd` before canonicalization. If
/// canonicalization fails (path may not exist, or symlink resolution errors),
/// return the pre-canonical absolute path so behavior for nonexistent paths is
/// preserved: a raw lexical prefix comparison against a stale/nonexistent path
/// just misses, which is the existing "All workspaces" fallback.
fn resolve_selector_path(selector: &Path, cwd: Option<&Path>) -> PathBuf {
    let absolute = if selector.is_absolute() {
        selector.to_path_buf()
    } else {
        match cwd {
            Some(cwd) => cwd.join(selector),
            None => selector.to_path_buf(),
        }
    };
    absolute.canonicalize().unwrap_or(absolute)
}

/// Upper bound on graceful connection drain once a shutdown signal (ctrl-c or
/// SIGTERM) is received — well under `orbit-web.service`'s
/// `TimeoutStopUSec=90s` (see the 2026-09-05 restart incident, ORB-11246:
/// `stop-sigterm` timed out and systemd fell back to SIGKILL). [`shutdown_signal`]
/// also tells long-lived streaming handlers (`api::request_shutdown`, e.g.
/// `/api/log/stream`) to close cooperatively as soon as shutdown begins, so in
/// practice the drain below finishes almost immediately; this timeout is a
/// deterministic backstop for a connection that doesn't cooperate, so the
/// process still exits on its own instead of relying on systemd's SIGKILL.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(10);

/// Build the axum app and block on the tokio runtime until graceful shutdown.
fn run_server(args: &ServeArgs, state: state::DashboardState) -> Result<(), OrbitError> {
    check_bindable_host(args.host, args.port)?;

    let addr = SocketAddr::new(args.host, args.port);
    let url = format!("http://{addr}");
    let no_open = args.no_open;

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/static/dashboard.css", get(serve_dashboard_css))
        .route("/static/fonts/inter-latin.woff2", get(serve_inter_font))
        .route(
            "/static/fonts/jetbrains-mono-latin.woff2",
            get(serve_jetbrains_mono_font),
        )
        .route("/static/marked.umd.js", get(serve_marked_js))
        .route("/static/purify.min.js", get(serve_purify_js))
        .route("/static/app.js", get(serve_app_js))
        .route("/static/common.js", get(serve_common_js))
        .route("/static/markdown.js", get(serve_markdown_js))
        .route("/static/tasks.js", get(serve_tasks_js))
        .route("/static/field-editor.js", get(serve_field_editor_js))
        .route("/static/audit.js", get(serve_audit_js))
        .route("/static/scoreboard.js", get(serve_scoreboard_js))
        .route("/static/reliability.js", get(serve_reliability_js))
        .route("/static/log-tail.js", get(serve_log_tail_js))
        .route("/static/diagnostics.js", get(serve_diagnostics_js))
        .route("/static/router.js", get(serve_router_js))
        .route("/static/runs.js", get(serve_runs_js))
        .route("/static/run-detail.js", get(serve_run_detail_js))
        .route("/static/operations.js", get(serve_operations_js))
        .route("/static/automation.js", get(serve_automation_js))
        .route("/healthz", get(health::healthz))
        .nest("/api", api::router())
        .with_state(state);

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| OrbitError::Execution(format!("tokio runtime: {e}")))?;

    tokio_runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| OrbitError::Io(format!("bind {addr}: {e}")))?;

        #[allow(clippy::print_stdout)]
        {
            println!("Dashboard listening on {url}");
        }

        if !no_open {
            open_browser(&url);
        }

        // Shared with `drain_with_grace_period` below: the grace-period timer
        // must not start until shutdown is actually requested, so the signal
        // handler notifies it rather than the drain deadline starting
        // unconditionally when serving starts (see ORB-11255). `notify_one`
        // (not `notify_waiters`) is required here: `drain` (polled as part of
        // the `select!` in `drain_with_grace_period`) can resolve this signal
        // and reach this call before `grace_elapsed`'s `notified().await` has
        // ever been polled for the first time -- `notify_waiters` only wakes
        // *already-registered* waiters and would silently drop that
        // notification, leaving the grace timer never started. `notify_one`
        // stores a permit for exactly this case: a `notified().await` that
        // starts after the notify already fired consumes it immediately.
        // There is exactly one waiter (`grace_elapsed`), so `notify_one` is
        // sufficient.
        let shutdown_notify = Arc::new(Notify::new());
        let notify_on_signal = Arc::clone(&shutdown_notify);
        let shutdown = async move {
            shutdown_signal().await;
            // Ask cooperating long-lived connections (the `/api/log/stream`
            // SSE handler) to close now, before the bounded drain deadline
            // below is reached.
            api::request_shutdown();
            notify_on_signal.notify_one();
        };
        let drain = axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .into_future();

        drain_with_grace_period(drain, shutdown_notify, SHUTDOWN_GRACE_PERIOD).await
    })
}

/// Race a server-drain future against a grace-period timeout that only
/// starts counting down once `shutdown_notify` fires. Returns `drain`'s
/// result if it finishes first (normal completion of graceful shutdown, or a
/// serve error). Otherwise, once `grace_period` elapses after shutdown was
/// signaled, logs a warning and returns `Ok(())` so the process exits without
/// waiting further for connections that never close on their own.
///
/// Critically, `grace_elapsed` cannot resolve before `shutdown_notify` fires:
/// this is what stops a healthy, unsignaled server from being torn down after
/// `grace_period` elapses (ORB-11255 -- the prior `tokio::time::timeout`
/// wrapped the whole drain and started counting down when serving began, not
/// when shutdown was requested).
///
/// Callers must signal `shutdown_notify` with [`Notify::notify_one`], not
/// `notify_waiters`: `drain` is polled as part of the `select!` below and may
/// resolve the signal and notify before `grace_elapsed`'s `notified().await`
/// is ever polled for the first time. `notify_one` stores a permit for that
/// case; `notify_waiters` would silently drop the notification and leave the
/// grace timer never started.
async fn drain_with_grace_period(
    drain: impl Future<Output = std::io::Result<()>>,
    shutdown_notify: Arc<Notify>,
    grace_period: Duration,
) -> Result<(), OrbitError> {
    let grace_elapsed = async {
        shutdown_notify.notified().await;
        tokio::time::sleep(grace_period).await;
    };

    tokio::select! {
        result = drain => result.map_err(|e| OrbitError::Execution(format!("serve: {e}"))),
        () = grace_elapsed => {
            tracing::warn!(
                grace_period_secs = grace_period.as_secs(),
                "dashboard shutdown grace period elapsed with connections \
                 still open; exiting without waiting further"
            );
            Ok(())
        }
    }
}

/// Reject binding the dashboard to anything other than a loopback address.
///
/// SECURITY (ORB-00360): the dashboard has no authentication of its own. The
/// only request-level check is [`api::require_localhost_origin`], a
/// browser-CSRF mitigation that inspects the client-supplied `Origin` header
/// and is trivially spoofable by any non-browser client (curl, a LAN script).
/// It is NOT an access-control boundary. Binding to a non-loopback address
/// would expose the full unauthenticated read/write API to the network, so we
/// refuse. For remote access, bind loopback and front the dashboard with an
/// authenticated tunnel/reverse proxy (e.g. `ssh -L`).
fn check_bindable_host(host: IpAddr, port: u16) -> Result<(), OrbitError> {
    if host.is_loopback() {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "refusing to bind dashboard to non-loopback address {host}: the \
         dashboard is unauthenticated and the Origin check is not an \
         access-control boundary. Bind a loopback address (127.0.0.1 or ::1) \
         and use an authenticated tunnel/reverse proxy (e.g. \
         `ssh -L {port}:localhost:{port} <host>`) for remote access."
    )))
}

async fn serve_index() -> Response {
    dashboard_response("text/html; charset=utf-8", INDEX_HTML)
}

async fn serve_dashboard_css() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/css; charset=utf-8"),
        )],
        DASHBOARD_CSS,
    )
        .into_response()
}

async fn serve_inter_font() -> Response {
    dashboard_bytes_response("font/woff2", INTER_FONT)
}

async fn serve_jetbrains_mono_font() -> Response {
    dashboard_bytes_response("font/woff2", JETBRAINS_MONO_FONT)
}

async fn serve_marked_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", MARKED_JS)
}

async fn serve_purify_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", PURIFY_JS)
}

async fn serve_app_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", APP_JS)
}

async fn serve_common_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", COMMON_JS)
}

async fn serve_markdown_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", MARKDOWN_JS)
}

async fn serve_tasks_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", TASKS_JS)
}

async fn serve_field_editor_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", FIELD_EDITOR_JS)
}

async fn serve_audit_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", AUDIT_JS)
}

async fn serve_scoreboard_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", SCOREBOARD_JS)
}

async fn serve_reliability_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", RELIABILITY_JS)
}

async fn serve_log_tail_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", LOG_TAIL_JS)
}

async fn serve_diagnostics_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", DIAGNOSTICS_JS)
}

async fn serve_router_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", ROUTER_JS)
}

async fn serve_runs_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", RUNS_JS)
}

async fn serve_run_detail_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", RUN_DETAIL_JS)
}

async fn serve_operations_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", OPERATIONS_JS)
}
async fn serve_automation_js() -> Response {
    dashboard_response("application/javascript; charset=utf-8", AUTOMATION_JS)
}

fn dashboard_response(content_type: &'static str, body: &'static str) -> Response {
    dashboard_content_response(content_type, body)
}

fn dashboard_bytes_response(content_type: &'static str, body: &'static [u8]) -> Response {
    dashboard_content_response(content_type, body)
}

fn dashboard_content_response<T>(content_type: &'static str, body: T) -> Response
where
    T: IntoResponse,
{
    let mut response = (
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    );
    response
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

pub(crate) fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    #[cfg(windows)]
    let cmd = "explorer";

    let _ = std::process::Command::new(cmd)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
