use super::*;

/// A request-pinned view of dashboard state: one immutable [`Snapshot`]
/// generation plus shared access to the runtime cache. Every read and every
/// runtime resolution is evaluated against this single snapshot, so one response
/// never mixes old entry metadata with a runtime resolved from a newer binding.
pub(crate) struct Pinned {
    pub(super) inner: Arc<StateInner>,
    pub(super) snapshot: Arc<Snapshot>,
}

impl Pinned {
    /// The pinned generation's servable workspace entries.
    pub(crate) fn entries(&self) -> &[WsEntry] {
        &self.snapshot.entries
    }

    /// The pinned generation's default-workspace selection.
    pub(crate) fn default_workspace(&self) -> Option<&str> {
        self.snapshot.default_workspace.as_deref()
    }

    /// Resolve the runtime for `id` against the pinned snapshot's exact binding.
    pub(crate) fn runtime_for(&self, id: &str) -> Result<Arc<OrbitRuntime>, WsRejection> {
        self.inner.resolve_runtime(&self.snapshot, id)
    }

    /// Open runtimes whose binding matches the pinned snapshot, in snapshot
    /// order — the coherent open set for this request's generation.
    pub(crate) fn open_runtimes(&self) -> Vec<(String, Arc<OrbitRuntime>)> {
        self.inner.open_runtimes_for(&self.snapshot)
    }
}

/// The structured, credential-safe diagnostic emitted when a registry refresh
/// fails. It carries only the registry *path* and Orbit's own error text —
/// never the file contents — so a tokenized `git_remote` in the registry cannot
/// leak into logs. Extracted so its fields are unit-testable without a
/// subscriber and so the `warn!` call site emits exactly these two values.
pub(crate) struct RefreshFailure {
    registry: String,
    error: String,
}

impl RefreshFailure {
    pub(super) fn new(registry_path: &Path, error: &OrbitError) -> Self {
        Self {
            registry: registry_path.display().to_string(),
            error: error.to_string(),
        }
    }

    pub(super) fn warn(&self) {
        tracing::warn!(
            registry = %self.registry,
            error = %self.error,
            "workspace registry refresh failed; retaining last valid workspace set"
        );
    }
}

/// Rejection returned by the [`Ws`] extractor when a workspace cannot be
/// selected or built. Renders as a JSON `{ "error": ... }` body.
#[derive(Debug)]
pub(crate) struct WsRejection {
    status: StatusCode,
    message: String,
}

impl WsRejection {
    pub(super) fn unknown(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown workspace: {id}"),
        }
    }

    pub(super) fn inactive(id: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("workspace '{id}' is inactive; select an active workspace"),
        }
    }

    pub(super) fn build_failed(id: &str, err: &OrbitError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to open workspace '{id}': {err}"),
        }
    }

    pub(super) fn missing_binding(id: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("active workspace '{id}' has no runtime binding"),
        }
    }

    fn no_default() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "no workspace selected and no default is configured; \
                      pass ?workspace=<id>"
                .to_string(),
        }
    }
}

impl IntoResponse for WsRejection {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Extractor yielding the `Arc<OrbitRuntime>` for the request's workspace.
///
/// Selection order: the `?workspace=<id>` query parameter, else the state's
/// configured default. Handlers destructure it as `Ws(runtime)` — a drop-in
/// replacement for the former `State(runtime): State<Arc<OrbitRuntime>>`.
pub(crate) struct Ws(pub(crate) Arc<OrbitRuntime>);

#[axum::async_trait]
impl FromRequestParts<DashboardState> for Ws {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        // Pin one snapshot so selection and runtime resolution share a
        // generation: a native add/remove/rebind that rewrote workspaces.json
        // since the last request is honored, and the resolved runtime always
        // matches the pinned binding.
        let requested = parts.uri.query().and_then(workspace_from_query);
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let pinned = state.pin();
            let id = match requested {
                Some(id) => id,
                None => pinned
                    .default_workspace()
                    .map(str::to_string)
                    .ok_or_else(WsRejection::no_default)?,
            };
            Ok(Ws(pinned.runtime_for(&id)?))
        })
        .await
        .map_err(|error| WsRejection {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("workspace selection panicked: {error}"),
        })?
    }
}

/// Extract the `workspace` value from a raw query string (percent-decoded),
/// ignoring empty values so `?workspace=` behaves like an omitted parameter.
fn workspace_from_query(query: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "workspace")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}
