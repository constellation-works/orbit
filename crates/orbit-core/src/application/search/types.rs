use std::str::FromStr;

use serde::Serialize;

use crate::runtime::workspace::catalog::WorkspaceScope;

use super::{DEFAULT_LIMIT, MAX_LIMIT};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalSearchKind {
    Task,
    Friction,
    #[default]
    All,
}

impl GlobalSearchKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Friction => "friction",
            Self::All => "all",
        }
    }

    pub(super) fn includes_tasks(self) -> bool {
        matches!(self, Self::Task | Self::All)
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
            "friction" => Ok(Self::Friction),
            "all" => Ok(Self::All),
            other => Err(format!(
                "invalid search kind `{other}`; expected one of: task, friction, all"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GlobalSearchMode {
    Lexical,
}

#[derive(Debug, Clone, Default)]
pub struct GlobalSearchParams {
    pub query: Option<String>,
    pub kind: GlobalSearchKind,
    pub limit: usize,
    /// AND-filter by tag. Repeat for multi-tag AND semantics. Applies to
    /// task and friction (and `all`).
    pub tags: Vec<String>,
    /// Include normally-hidden statuses for the queried kind(s). Mutually
    /// overridden by `status`.
    pub all: bool,
    /// Explicit per-kind status override (set semantics). When non-empty,
    /// takes precedence over the `all` widener.
    pub status: Vec<String>,
    /// Task applicability filter using selector-mapping against `context_files`.
    pub path: Option<String>,
    /// Which workspaces this query covers. Defaults to
    /// [`WorkspaceScope::Current`], the untouched single-workspace path
    /// [ORB-11027].
    pub workspaces: WorkspaceScope,
}

impl GlobalSearchParams {
    /// The requested limit, capped at `MAX_LIMIT`. Zero means unset: a
    /// friction listing then returns up to the cap, any other search
    /// `DEFAULT_LIMIT`.
    pub fn normalized_limit(&self) -> usize {
        match self.limit {
            0 if self.is_friction_listing() => MAX_LIMIT,
            0 => DEFAULT_LIMIT,
            limit => limit.min(MAX_LIMIT),
        }
    }

    /// A `kind: friction` call with no query and no path lists the friction
    /// records the status and tag filters admit, in `created_at` then ID
    /// order, across every status unless a `friction:` status narrows it.
    pub fn is_friction_listing(&self) -> bool {
        self.kind == GlobalSearchKind::Friction
            && self.path.is_none()
            && self
                .query
                .as_deref()
                .is_none_or(|query| query.trim().is_empty())
    }
}

/// Explain multi-word semantics for an empty or partially matched search.
pub(crate) fn whitespace_query_note(query: &str, kind: GlobalSearchKind) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.split_whitespace().count() < 2 {
        return None;
    }
    let mut semantics = Vec::new();
    if kind.includes_tasks() {
        semantics.push("task search matches non-adjacent indexed terms: all terms first, then any term to fill the limit. Partial labels count terms in the best matching chunk. Bundle-only fields use a case-insensitive substring; an unavailable index uses only that fallback");
    }
    if kind.includes_frictions() {
        semantics.push("friction search matches a single case-insensitive substring, so words must be adjacent; retry a distinctive term if the phrase has no hits");
    }
    Some(format!("query {trimmed:?}: {}.", semantics.join("; ")))
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalSearchResponse {
    pub mode: GlobalSearchMode,
    pub kind: GlobalSearchKind,
    pub results: Vec<GlobalSearchHit>,
    pub notes: Vec<String>,
    /// Kinds a `--path` query could not apply to (frictions are not
    /// path-filtered). Mirrors the "branch skipped" note in `notes`, but as a
    /// structured field an agent can check without parsing prose, so an empty
    /// `results` from a path query is not misread as "nothing relevant"
    /// [ORB-12259].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_kinds: Vec<String>,
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
    pub score_breakdown: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_by: Option<Vec<String>>,
    /// Set only on a federated query. `None` on the single-workspace path
    /// keeps that response byte-identical to before [ORB-11027].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<HitWorkspace>,
    /// The full friction record, set only on a friction listing so a caller
    /// triaging the set reads tags, reporter, task and disposition without a
    /// second lookup per hit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<serde_json::Value>,
}
