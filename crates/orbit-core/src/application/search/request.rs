//! Shared search request policy: one operation, named per-surface profiles.
//!
//! Surfaces retain only wire decoding (JSON/tool input, CLI matches, HTTP
//! query string) and result rendering. This module owns the defaults,
//! limit handling, scope construction, retired-parameter refusals and the
//! noun CLI's nonempty-query precondition that used to be copied in the
//! tool dispatch and the HTTP adapter.

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::runtime::workspace::catalog::WorkspaceScope;

use super::{GlobalSearchKind, GlobalSearchParams, GlobalSearchResponse};

/// HTTP list ceiling, matching the dashboard's other list endpoints.
const HTTP_MAX_LIMIT: usize = 200;
/// HTTP default page size when `limit` is omitted.
const HTTP_DEFAULT_LIMIT: usize = 10;

/// Which surface a decoded search request arrived from. Profiles preserve
/// each surface's existing limits, accepted fields and refusal semantics;
/// they do not converge the wire contracts themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRequestProfile {
    /// The noun CLI (`orbit search <query>`): tool limits, plus a required
    /// nonempty query distinct from tool/listing use.
    NounCli,
    /// The generic tool / MCP registry path (`orbit.search`): zero means
    /// unset, the engine caps at its maximum, and tag-only or queryless
    /// friction-listing forms are permitted.
    Tool,
    /// The dashboard HTTP adapter: default 10, explicit limits capped at
    /// 200, a single bound workspace, and the legacy `path` applicability
    /// filter preserved.
    Http,
}

/// A search request after surface-specific wire decoding, before policy.
///
/// `limit` is the explicitly requested limit, or `None` when omitted; the
/// profile decides what omission and zero mean. `path` is `Some` only on
/// surfaces whose contract accepts it (HTTP); the tool spec rejects the
/// retired field before this point.
#[derive(Debug, Clone, Default)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub kind: GlobalSearchKind,
    pub limit: Option<usize>,
    pub tags: Vec<String>,
    pub all: bool,
    pub status: Vec<String>,
    pub path: Option<String>,
    pub workspace_selectors: Vec<String>,
    pub all_workspaces: bool,
}

impl SearchRequestProfile {
    /// Apply this profile's policy, producing the engine parameters.
    pub fn search_params(self, request: SearchRequest) -> Result<GlobalSearchParams, OrbitError> {
        if self == Self::NounCli {
            self.require_nonempty_query(request.query.as_deref())?;
        }
        let limit = match self {
            // Zero preserves the engine's unset handling (default, or the
            // cap for a friction listing); the engine applies its maximum.
            Self::NounCli | Self::Tool => request.limit.unwrap_or(0),
            // Omitted defaults to the page size; an explicit value —
            // including zero, which the engine then treats as unset — is
            // capped at the dashboard ceiling.
            Self::Http => match request.limit {
                None => HTTP_DEFAULT_LIMIT,
                Some(limit) => limit.min(HTTP_MAX_LIMIT),
            },
        };
        Ok(GlobalSearchParams {
            query: request.query,
            kind: request.kind,
            limit,
            tags: request.tags,
            all: request.all,
            status: request.status,
            path: request.path,
            workspaces: WorkspaceScope::from_inputs(
                request.workspace_selectors,
                request.all_workspaces,
            ),
        })
    }

    /// The noun CLI's precondition: a nonempty query, unlike the tool's
    /// tag-only and friction-listing forms.
    pub fn require_nonempty_query(self, query: Option<&str>) -> Result<(), OrbitError> {
        if query.is_some_and(|query| !query.trim().is_empty()) {
            Ok(())
        } else {
            Err(OrbitError::InvalidInput(
                "search requires a query. Usage: orbit search <query>".into(),
            ))
        }
    }

    /// The HTTP adapter's lexical retired-parameter refusal, checked in
    /// query-string order during decoding so error precedence is unchanged.
    pub fn http_retired_parameter(self, key: &str) -> Option<String> {
        match key {
            "hybrid" | "semantic" => Some(format!("unknown search parameter `{key}`")),
            _ => None,
        }
    }
}

impl OrbitRuntime {
    /// The shared search operation every surface reaches: apply the
    /// profile's request policy, then run the existing search engine.
    pub fn search_with_profile(
        &self,
        profile: SearchRequestProfile,
        request: SearchRequest,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        self.global_search(profile.search_params(request)?)
    }
}
