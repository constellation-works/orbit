use std::str::FromStr;

use serde::Serialize;

use orbit_search::ScoreBreakdown;

use crate::runtime::workspace_catalog::WorkspaceScope;

use super::DEFAULT_LIMIT;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalSearchKind {
    Task,
    Doc,
    Friction,
    #[default]
    All,
}

impl GlobalSearchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Doc => "doc",
            Self::Friction => "friction",
            Self::All => "all",
        }
    }

    pub(super) fn includes_tasks(self) -> bool {
        matches!(self, Self::Task | Self::All)
    }

    pub(super) fn includes_docs(self) -> bool {
        matches!(self, Self::Doc | Self::All)
    }

    pub(super) fn includes_frictions(self) -> bool {
        matches!(self, Self::Friction | Self::All)
    }
}

impl FromStr for GlobalSearchKind {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "task" => Ok(Self::Task),
            "doc" => Ok(Self::Doc),
            "friction" => Ok(Self::Friction),
            "all" => Ok(Self::All),
            other => Err(format!(
                "invalid search kind `{other}`; expected one of: task, doc, friction, all"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalSearchMode {
    Lexical,
    Hybrid,
    Neighbor,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalSearchParams {
    pub query: Option<String>,
    // ADR-0179: hybrid free-text ranking and task-neighbor lookup are distinct modes.
    pub hybrid: bool,
    pub semantic: Option<String>,
    pub kind: GlobalSearchKind,
    pub limit: usize,
    /// AND-filter by tag. Repeat for multi-tag AND semantics. Applies to
    /// task, doc, and friction (and `all`).
    pub tags: Vec<String>,
    /// Include normally-hidden statuses for the queried kind(s). Mutually
    /// overridden by `status`.
    pub all: bool,
    /// Explicit per-kind status override (set semantics). When non-empty,
    /// takes precedence over the `all` widener.
    pub status: Vec<String>,
    /// Cross-kind applicability filter. Task: selector-mapping against
    /// `context_files`. Doc: out of scope (returns empty).
    pub path: Option<String>,
    /// Which workspaces this query covers. Defaults to
    /// [`WorkspaceScope::Current`], the untouched single-workspace path
    /// [ORB-11027].
    pub workspaces: WorkspaceScope,
}

impl GlobalSearchParams {
    pub fn normalized_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

/// Note attached when a whitespace-containing query matches nothing.
///
/// Lexical friction and task search keep a single case-insensitive substring
/// needle. A zero-hit multi-word query is therefore not evidence the corpus is
/// empty — the same records may still match each token on its own.
pub(crate) fn empty_whitespace_query_note(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    Some(format!(
        "query {trimmed:?} is a single case-insensitive substring (all tokens required together), not independent terms. Zero hits is not proof the corpus is empty; retry each distinctive token on its own: {}.",
        tokens.join(", ")
    ))
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalSearchResponse {
    pub mode: GlobalSearchMode,
    pub kind: GlobalSearchKind,
    pub results: Vec<GlobalSearchHit>,
    pub notes: Vec<String>,
    /// Per-workspace outcome of a federated query. Empty — and omitted from
    /// JSON — for the default single-workspace scope, so an existing caller
    /// sees the same response shape it always did.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<WorkspaceSearchReport>,
}

/// What one workspace contributed to a federated query.
///
/// A workspace that answered nothing still appears here with `hits: 0` and a
/// `note`, so "returned no matches" is distinguishable from "was never asked".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceSearchReport {
    pub workspace_id: String,
    pub name: String,
    pub hits: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Which workspace a hit came from.
///
/// Load-bearing, not decorative: task IDs are globally unique and resolve
/// through the host registry, but friction and job-run IDs are allocated per
/// workspace, so the same ID names a different record in each. F2026-08-046
/// records a near-miss write to the wrong record from exactly that ambiguity
/// in a merged result set [ORB-11027].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HitWorkspace {
    pub workspace_id: String,
    pub name: String,
    pub repo_root: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalSearchHit {
    pub kind: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_breakdown: Option<ScoreBreakdown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<Vec<String>>,
    /// Set only on a federated query. `None` on the single-workspace path
    /// keeps that response byte-identical to before [ORB-11027].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<HitWorkspace>,
}
