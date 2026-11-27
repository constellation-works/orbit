//! Dashboard task and run permalinks, resolved through the host registry.
//!
//! `GET /t/<task id>` and `GET /r/<run id>` redirect to the dashboard's own task
//! and run views, naming the host every time.
//!
//! A link written into a transcript, a pull request or a mail must open the
//! record it names whichever host the browser last selected. The dashboard
//! restores that last remote host when a URL carries no `host`, and an explicit
//! empty `host=` selects the serving machine, so every redirect here sets
//! `host`. Resolution reads the serving machine's host registry snapshot, as
//! the CLI's task routing does; it contacts no host. The routes sit behind the
//! same Host, Origin and Fetch Metadata gate as the dashboard shell, and the
//! redirect is origin-relative so it holds at any address the dashboard is
//! served from.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use orbit_registry::hosts::{HostRegistry, TaskPrefixRoute};
use orbit_types::task::is_valid_orb_task_id;
use serde::Deserialize;
use url::form_urlencoded;

use crate::assets::DASHBOARD_CSP;
use crate::state::DashboardState;

/// Longest run id accepted; real ids are about 25 characters.
const MAX_RUN_ID_LEN: usize = 128;

#[derive(Debug, Deserialize)]
struct RunQuery {
    /// The stable `[machine] id` of the host the run executed on.
    machine: Option<String>,
    /// The workspace on that host; `all` when absent.
    workspace: Option<String>,
}

/// What a permalink resolved to.
#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    /// An origin-relative dashboard location.
    Redirect(String),
    /// A named not-found result.
    NotFound { title: String, detail: String },
}

pub(crate) fn router() -> Router<DashboardState> {
    Router::new()
        .route("/t/{id}", get(task))
        .route("/r/{id}", get(run))
}

/// `GET /t/<id>`: the task on the host its prefix names.
async fn task(State(state): State<DashboardState>, Path(id): Path<String>) -> Response {
    resolve_with_registry(state, move |registry| resolve_task(registry, &id)).await
}

/// `GET /r/<id>[?machine=<machine id>&workspace=<workspace>]`: that exact run,
/// or, without `machine`, a lookup on the serving machine only.
async fn run(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
    Query(query): Query<RunQuery>,
) -> Response {
    resolve_with_registry(state, move |registry| {
        resolve_run(
            registry,
            &id,
            non_empty(query.machine.as_deref()),
            non_empty(query.workspace.as_deref()),
        )
    })
    .await
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Run `resolve` against the last valid host registry snapshot, off the
/// request worker because pinning may read the host files.
async fn resolve_with_registry(
    state: DashboardState,
    resolve: impl FnOnce(&HostRegistry) -> Resolution + Send + 'static,
) -> Response {
    let resolved = tokio::task::spawn_blocking(move || {
        let pinned = state.hosts();
        match pinned.snapshot {
            Some(snapshot) => Ok(resolve(&snapshot.registry)),
            None => Err(pinned
                .load_error
                .map_or_else(|| "the host file is not loaded".to_string(), |e| e.message)),
        }
    })
    .await;
    match resolved {
        Ok(Ok(Resolution::Redirect(location))) => Redirect::temporary(&location).into_response(),
        Ok(Ok(Resolution::NotFound { title, detail })) => {
            page(StatusCode::NOT_FOUND, &title, &detail)
        }
        Ok(Err(message)) => page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Hosts unavailable",
            &format!("The host registry could not be read: {message}"),
        ),
        Err(error) => {
            tracing::error!(%error, "permalink resolution panicked");
            page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Link failed",
                "The link could not be resolved.",
            )
        }
    }
}

fn resolve_task(registry: &HostRegistry, id: &str) -> Resolution {
    let id = id.trim();
    if !is_valid_orb_task_id(id) {
        return Resolution::NotFound {
            title: format!("Task {id} not found"),
            detail: format!("{id} is not a task id."),
        };
    }
    match registry.task_prefix_table().route(id) {
        TaskPrefixRoute::Local => Resolution::Redirect(dashboard_location("", "all", "tasks", id)),
        TaskPrefixRoute::Host(entry) => {
            Resolution::Redirect(dashboard_location(&entry.name, "all", "tasks", id))
        }
        TaskPrefixRoute::Unregistered { prefix } => Resolution::NotFound {
            title: format!("Task {id} not found"),
            detail: format!("No host is registered for the prefix {prefix}."),
        },
    }
}

fn resolve_run(
    registry: &HostRegistry,
    id: &str,
    machine: Option<&str>,
    workspace: Option<&str>,
) -> Resolution {
    let id = id.trim();
    if !is_run_id(id) {
        return Resolution::NotFound {
            title: format!("Run {id} not found"),
            detail: format!("{id} is not a run id."),
        };
    }
    let workspace = workspace.unwrap_or("all");
    let Some(machine) = machine else {
        // No machine named: the serving machine, never another host's run.
        return Resolution::Redirect(dashboard_location("", workspace, "runs", id));
    };
    if registry.local().is_some_and(|local| local.id == machine) {
        return Resolution::Redirect(dashboard_location("", workspace, "runs", id));
    }
    // A machine this one has not registered is not found. It must not fall back
    // to a local lookup, which could open an unrelated run with the same id.
    match registry
        .hosts()
        .entries()
        .iter()
        .find(|entry| entry.machine_id == machine)
    {
        Some(entry) => Resolution::Redirect(dashboard_location(&entry.name, workspace, "runs", id)),
        None => Resolution::NotFound {
            title: format!("Run {id} not found"),
            detail: format!("Machine {machine} is not registered on this host."),
        },
    }
}

/// A run id is `jrun-<date>-<time>[-<role><sequence>]`, or an older bare stem
/// or number: ASCII letters, digits, `-` and `_`.
fn is_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RUN_ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// The dashboard URL for `view/id`, with `host` always present (empty selects
/// the serving machine). `id` is a validated id, so the fragment needs no escaping.
fn dashboard_location(host: &str, workspace: &str, view: &str, id: &str) -> String {
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair("host", host)
        .append_pair("workspace", workspace)
        .finish();
    format!("/?{query}#{view}/{id}")
}

/// A small standalone not-found or failure page naming what was asked for.
fn page(status: StatusCode, title: &str, detail: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title} · Orbit</title></head>\
         <body style=\"font-family:system-ui,sans-serif;margin:3rem auto;max-width:36rem;padding:0 1rem\">\
         <h1 style=\"font-size:1.25rem\">{title}</h1><p>{detail}</p>\
         <p><a href=\"/\">Open the dashboard</a></p></body></html>",
        title = escape_html(title),
        detail = escape_html(detail),
    );
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    );
    response
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}
