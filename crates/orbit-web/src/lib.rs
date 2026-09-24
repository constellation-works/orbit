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
mod runtime_memo;
mod ssh_tunnel;
mod state;

#[cfg(test)]
mod tests;

pub use connect::{ConnectArgs, connect};

use std::future::{Future, IntoFuture};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use clap::Args;
use flate2::{Compression, write::GzEncoder};
use orbit_cmd::registry_runtime;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_registry::workspace_registry;
use orbit_types::workspace::{WorkspaceRegistry, WorkspaceStatus};
use tokio::sync::Notify;

const HTML: &str = "text/html; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "application/javascript; charset=utf-8";
const WOFF2: &str = "font/woff2";

/// Every embedded dashboard file as `(route, content type, body)`.
// L-0021: Keep embedded dashboard modules in sync with the files they import.
const DASHBOARD_FILES: &[(&str, &str, &[u8])] = &[
    ("/", HTML, include_bytes!("../assets/dashboard/index.html")),
    (
        "/static/dashboard.css",
        CSS,
        include_bytes!("../assets/dashboard/dashboard.css"),
    ),
    (
        "/static/fonts/inter-latin.woff2",
        WOFF2,
        include_bytes!("../assets/dashboard/fonts/inter-latin.woff2"),
    ),
    (
        "/static/fonts/jetbrains-mono-latin.woff2",
        WOFF2,
        include_bytes!("../assets/dashboard/fonts/jetbrains-mono-latin.woff2"),
    ),
    (
        "/static/marked.umd.js",
        JS,
        include_bytes!("../assets/dashboard/marked.umd.js"),
    ),
    (
        "/static/purify.min.js",
        JS,
        include_bytes!("../assets/dashboard/purify.min.js"),
    ),
    (
        "/static/app.js",
        JS,
        include_bytes!("../assets/dashboard/app.js"),
    ),
    (
        "/static/common.js",
        JS,
        include_bytes!("../assets/dashboard/common.js"),
    ),
    (
        "/static/config.js",
        JS,
        include_bytes!("../assets/dashboard/config.js"),
    ),
    (
        "/static/markdown.js",
        JS,
        include_bytes!("../assets/dashboard/markdown.js"),
    ),
    (
        "/static/tasks.js",
        JS,
        include_bytes!("../assets/dashboard/tasks.js"),
    ),
    (
        "/static/field-editor.js",
        JS,
        include_bytes!("../assets/dashboard/field-editor.js"),
    ),
    (
        "/static/audit.js",
        JS,
        include_bytes!("../assets/dashboard/audit.js"),
    ),
    (
        "/static/scoreboard.js",
        JS,
        include_bytes!("../assets/dashboard/scoreboard.js"),
    ),
    (
        "/static/reliability.js",
        JS,
        include_bytes!("../assets/dashboard/reliability.js"),
    ),
    (
        "/static/log-tail.js",
        JS,
        include_bytes!("../assets/dashboard/log-tail.js"),
    ),
    (
        "/static/diagnostics.js",
        JS,
        include_bytes!("../assets/dashboard/diagnostics.js"),
    ),
    (
        "/static/router.js",
        JS,
        include_bytes!("../assets/dashboard/router.js"),
    ),
    (
        "/static/runs.js",
        JS,
        include_bytes!("../assets/dashboard/runs.js"),
    ),
    (
        "/static/run-detail.js",
        JS,
        include_bytes!("../assets/dashboard/run-detail.js"),
    ),
    (
        "/static/distributed.js",
        JS,
        include_bytes!("../assets/dashboard/distributed.js"),
    ),
    (
        "/static/operations.js",
        JS,
        include_bytes!("../assets/dashboard/operations.js"),
    ),
    (
        "/static/automation.js",
        JS,
        include_bytes!("../assets/dashboard/automation.js"),
    ),
    (
        "/static/plugins.js",
        JS,
        include_bytes!("../assets/dashboard/plugins.js"),
    ),
];
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
const DASHBOARD_CACHE_CONTROL: &str = "no-cache";

struct DashboardAsset {
    content_type: &'static str,
    body: &'static [u8],
    gzip_body: Bytes,
    etag: HeaderValue,
}

impl DashboardAsset {
    fn new(content_type: &'static str, body: &'static [u8]) -> Result<Self, OrbitError> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(body)
            .map_err(|error| OrbitError::Execution(format!("gzip dashboard asset: {error}")))?;
        let gzip_body = encoder.finish().map_err(|error| {
            OrbitError::Execution(format!("finish gzip dashboard asset: {error}"))
        })?;
        let digest = blake3::hash(body);
        let etag = HeaderValue::from_str(&format!(r#""{}""#, digest.to_hex()))
            .map_err(|error| OrbitError::Execution(format!("dashboard asset ETag: {error}")))?;

        Ok(Self {
            content_type,
            body,
            gzip_body: Bytes::from(gzip_body),
            etag,
        })
    }
}

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

    /// Grant operator capability for the dashboard's governed controls —
    /// Operations, and the owner's handoff approve/revoke/recover actions —
    /// without a TTY or ORBIT_OPERATOR. `orbit web connect` passes this by
    /// default.
    #[arg(long)]
    pub operator: bool,
}

/// Boot the dashboard for a single, already-built runtime and block until
/// shutdown (ctrl-c or SIGTERM). Single-workspace mode is no longer reachable
/// from `orbit web serve` (see [`serve_from_env`], ORB-10029); this stays for
/// callers that already hold an `OrbitRuntime` and want it embedded directly.
pub fn serve(runtime: &OrbitRuntime, args: ServeArgs) -> Result<(), OrbitError> {
    let state = state::DashboardState::single(Arc::new(runtime.clone()));
    state.set_operator_session(args.operator);
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
    state.set_operator_session(args.operator);
    run_server(&args, state)
}

/// Resolve dashboard state from the environment: registry-backed global mode
/// over every workspace registered in the served registry (stale-path entries
/// are listed but marked inactive and never built). The registry is
/// `<root>/workspaces.json` for an explicit `--root`, and the machine-global
/// `~/.orbit/workspaces.json` otherwise — the same resolution every other
/// root-aware command performs, via
/// [`orbit_cmd::registry_runtime::global_root_for`]. The servable set is
/// reloaded from that same path when `workspaces.json` mtime or length
/// changes (see [`state::DashboardState::pin`]), so a native `orbit
/// workspace init/remove` or binding change becomes visible on the next
/// request without restarting the server. The dropdown's default selection is,
/// in priority order: the registered/active workspace matching
/// `workspace_selector` (an explicit
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

/// `/healthz` with the same Host/Origin and nosniff layers as `/api`.
///
/// The detailed form returns workspace names and the log-sink path, so it
/// must not skip the DNS-rebinding Host gate that `/api` already has
/// (ORB-12531). Static dashboard assets stay on the outer router and are
/// unchanged.
pub(crate) fn health_router() -> Router<state::DashboardState> {
    Router::new()
        .route("/healthz", get(health::healthz))
        .layer(middleware::from_fn(api::require_localhost_origin))
        // Outer so Host/Origin 403s and handler bodies both carry nosniff.
        .layer(middleware::map_response(api::nosniff_json_responses))
}

/// Build the axum app and block on the tokio runtime until graceful shutdown.
fn run_server(args: &ServeArgs, state: state::DashboardState) -> Result<(), OrbitError> {
    check_bindable_host(args.host, args.port)?;

    let addr = SocketAddr::new(args.host, args.port);
    let url = format!("http://{addr}");
    let no_open = args.no_open;
    let app = dashboard_file_router()?
        .merge(health_router())
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
/// SECURITY (ORB-00360): the dashboard has no authentication of its own.
/// Request-level checks in [`api::require_localhost_origin`] mitigate browser
/// CSRF (`Origin`, ORB-11613) and DNS rebinding (`Host`, ORB-12506). Both
/// inspect client-supplied headers and are trivially spoofable by any
/// non-browser client (curl, a LAN script). They are NOT an access-control
/// boundary. Binding to a non-loopback address would expose the full
/// unauthenticated read/write API to the network, so we refuse. For remote
/// access, bind loopback and front the dashboard with an authenticated
/// tunnel/reverse proxy (e.g. `ssh -L`).
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

/// One route per embedded dashboard file, each serving its precompressed,
/// ETag-validated asset.
fn dashboard_file_router() -> Result<Router<state::DashboardState>, OrbitError> {
    DASHBOARD_FILES
        .iter()
        .try_fold(Router::new(), |router, &(route, content_type, body)| {
            let asset = Arc::new(DashboardAsset::new(content_type, body)?);
            Ok(router.route(
                route,
                get(move |headers: HeaderMap| {
                    let asset = Arc::clone(&asset);
                    async move { dashboard_asset_response(&asset, &headers) }
                }),
            ))
        })
}

fn dashboard_asset_response(asset: &DashboardAsset, request_headers: &HeaderMap) -> Response {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(DASHBOARD_CACHE_CONTROL),
    );
    response_headers.insert(header::ETAG, asset.etag.clone());
    response_headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    );
    response_headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));

    if if_none_match_matches(request_headers.get(header::IF_NONE_MATCH), &asset.etag) {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }

    if accepts_gzip(request_headers.get(header::ACCEPT_ENCODING)) {
        response_headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        return (response_headers, asset.gzip_body.clone()).into_response();
    }

    (response_headers, asset.body).into_response()
}

fn if_none_match_matches(value: Option<&HeaderValue>, etag: &HeaderValue) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(etag) = etag.to_str().ok() else {
        return false;
    };

    value.split(',').any(|candidate| {
        let candidate = candidate.trim();
        candidate == "*"
            || candidate == etag
            || candidate
                .strip_prefix("W/")
                .is_some_and(|weak_etag| weak_etag.trim() == etag)
    })
}

fn accepts_gzip(value: Option<&HeaderValue>) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };

    let mut gzip_quality = None;
    let mut wildcard_quality = None;
    for encoding in value.split(',') {
        let mut parts = encoding.split(';');
        let coding = parts.next().unwrap_or("").trim();
        let quality = parts
            .filter_map(|parameter| parameter.trim().split_once('='))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("q")
                    .then(|| value.trim().parse::<f32>().unwrap_or(0.0))
            })
            .unwrap_or(1.0);

        if coding.eq_ignore_ascii_case("gzip") {
            gzip_quality = Some(quality);
        } else if coding == "*" {
            wildcard_quality = Some(quality);
        }
    }

    gzip_quality.or(wildcard_quality).unwrap_or(0.0) > 0.0
}

/// Serve the embedded dashboard file at `route` as its route handler would.
#[cfg(test)]
fn serve_dashboard_file(route: &str, headers: &HeaderMap) -> Response {
    let &(_, content_type, body) = DASHBOARD_FILES
        .iter()
        .find(|(candidate, _, _)| *candidate == route)
        .unwrap_or_else(|| panic!("no embedded dashboard file is routed at {route}"));
    let asset = DashboardAsset::new(content_type, body)
        .unwrap_or_else(|error| panic!("build dashboard asset {route}: {error}"));
    dashboard_asset_response(&asset, headers)
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
