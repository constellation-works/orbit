use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_core::Core;
use orbit_core::application::agents::AgentFacts;
use orbit_core::application::codebase::CodebaseFacts;

use super::*;

/// Stable code for the environmental refusal when the selected workspace is
/// not active in the pinned registry snapshot. That state can change between
/// attempts and this rejection precedes any handler or receipt lookup, so a
/// retained replay key must not treat it as proof that no dispatch was
/// recorded. Mirrors the `workspace_unavailable` code in task creation.
pub(crate) const WORKSPACE_UNAVAILABLE_CODE: &str = "workspace_unavailable";

/// A request-pinned view of dashboard state: one immutable [`Snapshot`]
/// generation plus shared access to the runtime cache. Every read and every
/// runtime resolution is evaluated against this single snapshot, so one response
/// never mixes old entry metadata with a runtime resolved from a newer binding.
pub(crate) struct Pinned {
    pub(super) inner: Arc<StateInner>,
    pub(super) snapshot: Arc<Snapshot>,
}

impl Pinned {
    /// Whether another request has published a different registry generation.
    pub(crate) fn same_generation(&self, other: &Self) -> bool {
        self.snapshot.generation == other.snapshot.generation
    }

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

    /// The app entry for a workspace from this pinned snapshot.
    pub(crate) fn core_for(&self, id: &str) -> Result<Arc<Core>, WsRejection> {
        self.runtime_for(id).map(|runtime| WsCore::over(&runtime).0)
    }

    /// Stale-tolerant resolve for `GET /api/config/file` only; see
    /// [`crate::state::dashboard`] for why aggregates stay strict.
    pub(crate) fn runtime_for_config_file(
        &self,
        id: &str,
    ) -> Result<Arc<OrbitRuntime>, WsRejection> {
        self.inner
            .resolve_runtime_for_config_file(&self.snapshot, id)
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
    code: Option<&'static str>,
}

impl WsRejection {
    pub(super) fn unknown(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown workspace: {id}"),
            code: None,
        }
    }

    pub(super) fn inactive(id: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("workspace '{id}' is inactive; select an active workspace"),
            code: Some(WORKSPACE_UNAVAILABLE_CODE),
        }
    }

    pub(super) fn build_failed(id: &str, err: &OrbitError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to open workspace '{id}': {err}"),
            code: None,
        }
    }

    pub(super) fn missing_binding(id: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("active workspace '{id}' has no runtime binding"),
            code: None,
        }
    }

    pub(super) fn no_default() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "no workspace selected and no default is configured; \
                      pass ?workspace=<id>"
                .to_string(),
            code: None,
        }
    }
}

impl IntoResponse for WsRejection {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message });
        if let Some(code) = self.code {
            body["code"] = json!(code);
        }
        (self.status, Json(body)).into_response()
    }
}

/// Extractor yielding the `Arc<OrbitRuntime>` for the request's workspace.
///
/// Selection order: the `?workspace=<id>` query parameter, else the state's
/// configured default. Handlers destructure it as `Ws(runtime)` — a drop-in
/// replacement for the former `State(runtime): State<Arc<OrbitRuntime>>`.
pub(crate) struct Ws(pub(crate) Arc<OrbitRuntime>);

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
        // A warm request whose runtime is already cached current resolves
        // inline; anything that might reload or build takes the blocking
        // path below. See `DashboardState::try_resolve_cached`.
        if let Some(resolved) = state.try_resolve_cached(requested.as_deref()) {
            return resolved.map(Ws);
        }
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
            code: None,
        })?
    }
}

/// Extractor yielding the opaque [`Core`] of the request's workspace, for a
/// handler that calls an app's `exec` (`orbit_core::app::<subject>::exec`).
///
/// It selects the workspace exactly as [`Ws`] does and wraps the same runtime,
/// so a handler destructures it as `WsCore(core)` without naming the runtime.
pub(crate) struct WsCore(pub(crate) Arc<Core>);

impl WsCore {
    /// The opaque [`Core`] over an already resolved workspace runtime, for a
    /// handler that selected its workspace itself.
    pub(crate) fn over(runtime: &Arc<OrbitRuntime>) -> Self {
        Self(Arc::new(RegisteredRuntimeFactory::core_for_service(
            runtime.as_ref().clone(),
        )))
    }
}

impl FromRequestParts<DashboardState> for WsCore {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        Ok(Self::over(&runtime))
    }
}

/// A panel request's opaque Core and the cache identity of its selected runtime.
pub(crate) struct PluginPanelWs {
    pub(crate) core: Arc<Core>,
    runtime: Arc<OrbitRuntime>,
}

impl PluginPanelWs {
    /// Reuse cached panel output for this runtime generation.
    pub(crate) async fn read_cached(
        &self,
        state: &DashboardState,
        key: (String, String),
        ttl: std::time::Duration,
        read: impl FnOnce() -> Result<serde_json::Value, orbit_core::Error> + Send + 'static,
    ) -> Result<crate::runtime_memo::Served<serde_json::Value>, orbit_core::Error> {
        state
            .plugin_panel_memo()
            .get_or_compute(&self.runtime, key, ttl, read)
            .await
    }
}

impl FromRequestParts<DashboardState> for PluginPanelWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        let WsCore(core) = WsCore::over(&runtime);
        Ok(Self { core, runtime })
    }
}

/// An agent scorecard request's opaque [`Core`] and the cache identity of its
/// selected runtime.
pub(crate) struct AgentsWs {
    pub(crate) core: Arc<Core>,
    runtime: Arc<OrbitRuntime>,
}

impl AgentsWs {
    /// The cached agent scan for this runtime generation, served stale while
    /// one background `scan` replaces it.
    pub(crate) async fn cached_scan(
        &self,
        state: &DashboardState,
        scan: impl FnOnce() -> Result<AgentFacts, orbit_core::Error> + Send + 'static,
    ) -> Result<crate::runtime_memo::Served<AgentFacts>, orbit_core::Error> {
        state
            .agent_memo()
            .get_or_refresh(
                &self.runtime,
                (),
                crate::runtime_memo::AGENT_SCAN_TTL,
                crate::runtime_memo::AGENT_SCAN_STALE_LIMIT,
                scan,
            )
            .await
    }
}

impl FromRequestParts<DashboardState> for AgentsWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        let WsCore(core) = WsCore::over(&runtime);
        Ok(Self { core, runtime })
    }
}

/// A codebase analytics request's opaque [`Core`] and the cache identity of
/// its selected runtime.
pub(crate) struct CodebaseWs {
    pub(crate) core: Arc<Core>,
    runtime: Arc<OrbitRuntime>,
}

impl CodebaseWs {
    /// The cached history-only scan for this runtime generation, served stale
    /// while one background `scan` replaces it.
    pub(crate) async fn cached_scan(
        &self,
        state: &DashboardState,
        scan: impl FnOnce() -> Result<CodebaseFacts, orbit_core::Error> + Send + 'static,
    ) -> Result<crate::runtime_memo::Served<CodebaseFacts>, orbit_core::Error> {
        state
            .codebase_memo()
            .get_or_refresh(
                &self.runtime,
                None,
                crate::runtime_memo::CODEBASE_ANALYTICS_TTL,
                crate::runtime_memo::CODEBASE_ANALYTICS_STALE_LIMIT,
                scan,
            )
            .await
    }

    /// The cached related-file scan of one resolved `window`.
    pub(crate) async fn cached_related_scan(
        &self,
        state: &DashboardState,
        window: String,
        scan: impl FnOnce() -> Result<CodebaseFacts, orbit_core::Error> + Send + 'static,
    ) -> Result<crate::runtime_memo::Served<CodebaseFacts>, orbit_core::Error> {
        state
            .codebase_memo()
            .get_or_compute(
                &self.runtime,
                Some(window),
                crate::runtime_memo::CODEBASE_ANALYTICS_TTL,
                scan,
            )
            .await
    }
}

impl FromRequestParts<DashboardState> for CodebaseWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        let WsCore(core) = WsCore::over(&runtime);
        Ok(Self { core, runtime })
    }
}

/// An audit-summary request's opaque [`Core`] and the cache identity of its
/// selected runtime.
pub(crate) struct AuditSummaryWs {
    pub(crate) core: Arc<Core>,
    runtime: Arc<OrbitRuntime>,
}

impl AuditSummaryWs {
    /// Reuse a cached audit summary payload for this runtime generation.
    pub(crate) async fn read_cached(
        &self,
        state: &DashboardState,
        key: String,
        ttl: std::time::Duration,
        read: impl FnOnce() -> Result<serde_json::Value, orbit_core::Error> + Send + 'static,
    ) -> Result<crate::runtime_memo::Served<serde_json::Value>, orbit_core::Error> {
        state
            .audit_summary_memo()
            .get_or_compute(&self.runtime, key, ttl, read)
            .await
    }
}

impl FromRequestParts<DashboardState> for AuditSummaryWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let Ws(runtime) = Ws::from_request_parts(parts, state).await?;
        let WsCore(core) = WsCore::over(&runtime);
        Ok(Self { core, runtime })
    }
}

/// Stale-tolerant workspace extractor for `GET /api/config/file` only.
///
/// A broken layered config must stay repairable through the file view (400
/// with the file path), while every other endpoint — including fleet and
/// aggregate rows — reports the same breakage as an error/`unavailable`.
/// Handlers destructure it as `ConfigFileWs(core)`.
pub(crate) struct ConfigFileWs(pub(crate) Arc<Core>);

impl FromRequestParts<DashboardState> for ConfigFileWs {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        let requested = parts.uri.query().and_then(workspace_from_query);
        // Same inline fast path as `Ws`: the stale-tolerant resolver shares
        // its cached first step, and everything past it (a rebuild, the
        // stale-runtime fallback) stays on the blocking path below.
        if let Some(resolved) = state.try_resolve_cached(requested.as_deref()) {
            return resolved.map(|runtime| ConfigFileWs(WsCore::over(&runtime).0));
        }
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
            Ok(ConfigFileWs(
                WsCore::over(&pinned.runtime_for_config_file(&id)?).0,
            ))
        })
        .await
        .map_err(|error| WsRejection {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("workspace selection panicked: {error}"),
            code: None,
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
