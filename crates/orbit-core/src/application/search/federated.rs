//! Cross-workspace search: fan out over registered checkouts, fuse, attribute.
//!
//! Each workspace keeps owning and writing its own `.orbit/state/semantic.db`;
//! only the *read* federates [ORB-11027]. Core resolves nothing about the
//! catalog itself — a `WorkspaceCatalog` supplied by the registry-owning layer
//! answers "which checkouts" and "open this one", and everything below is
//! fan-out, fusion, attribution, and degradation notes.
//!
//! Two rules shape the fusion:
//!
//! * **Rank, never raw score.** Per-workspace hits are interleaved by their
//!   position in their own workspace's ranked list, reusing the same
//!   round-robin merge that already balances kinds. Lexical BM25 scores,
//!   blended hybrid scores, and `None` (frictions) are not commensurable
//!   across workspaces, so nothing compares them, and no single large
//!   workspace can crowd out the rest.
//! * **Every hit is attributed.** Friction and job-run IDs are allocated per
//!   workspace, so a merged list without a workspace field lets a caller route
//!   a follow-up write to the wrong record (F2026-08-046).

use std::collections::BTreeSet;

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::runtime::workspace_catalog::{
    FederatedWorkspaceTarget, WorkspaceCatalog, WorkspaceScope,
};

use super::merge_round_robin;
use super::types::{
    GlobalSearchHit, GlobalSearchMode, GlobalSearchParams, GlobalSearchResponse, HitWorkspace,
    WorkspaceSearchReport,
};

/// How many registered checkouts one federated query will open.
///
/// A bound exists because each workspace costs a runtime open plus a SQLite
/// handle. It is never applied silently: exceeding it adds a note naming both
/// the cap and how many workspaces were dropped.
pub(super) const MAX_FEDERATED_WORKSPACES: usize = 16;

const NO_MATCHING_WORKSPACE_NOTE: &str = "no registered workspace matched the requested scope";

#[cfg(test)]
thread_local! {
    /// Test seam for the managed-run guard. The suite itself may run inside an
    /// Orbit-managed job, whose environment would otherwise make every
    /// fan-out test observe the refusal.
    static MANAGED_RUN_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

fn in_managed_run() -> bool {
    #[cfg(test)]
    if let Some(value) = MANAGED_RUN_OVERRIDE.with(std::cell::Cell::get) {
        return value;
    }
    crate::runtime::run_input::managed_run_context_from_env()
}

#[cfg(test)]
pub(super) fn with_managed_run_override<T>(value: bool, f: impl FnOnce() -> T) -> T {
    MANAGED_RUN_OVERRIDE.with(|cell| cell.set(Some(value)));
    let out = f();
    MANAGED_RUN_OVERRIDE.with(|cell| cell.set(None));
    out
}

impl OrbitRuntime {
    pub(super) fn federated_search(
        &self,
        params: GlobalSearchParams,
    ) -> Result<GlobalSearchResponse, OrbitError> {
        ensure_federated_scope_supported(&params)?;
        ensure_federated_scope_permitted(in_managed_run())?;

        let catalog = self
            .workspace_catalog()
            .ok_or_else(federation_unavailable)?;

        let mut notes = Vec::new();
        let mut targets = catalog.resolve_scope(&params.workspaces)?;
        if let Some(note) = apply_workspace_cap(&mut targets) {
            notes.push(note);
        }
        if targets.is_empty() {
            notes.push(NO_MATCHING_WORKSPACE_NOTE.to_string());
            return Ok(GlobalSearchResponse {
                mode: GlobalSearchMode::Lexical,
                kind: params.kind,
                results: Vec::new(),
                notes,
                skipped_kinds: Vec::new(),
                workspaces: Vec::new(),
            });
        }

        // Resolved once: the query-side model is a host fact, not a
        // per-workspace one. Only hybrid ranking compares vectors, so a lexical
        // query never pays for it.
        let query_model = params
            .hybrid
            .then(|| orbit_search::query_model_id(None).ok())
            .flatten();

        let mut branches = Vec::with_capacity(targets.len());
        let mut reports = Vec::with_capacity(targets.len());
        let mut vector_ran = false;
        for target in &targets {
            let (hits, report, mode) = self.query_one_workspace(
                catalog.as_ref(),
                target,
                &params,
                query_model.as_deref(),
                &mut notes,
            );
            vector_ran |= mode == GlobalSearchMode::Hybrid;
            branches.push(hits);
            reports.push(report);
        }

        let results = merge_round_robin(branches, params.normalized_limit());
        let mode = if params.hybrid && vector_ran {
            GlobalSearchMode::Hybrid
        } else {
            GlobalSearchMode::Lexical
        };
        Ok(GlobalSearchResponse {
            mode,
            kind: params.kind,
            results,
            notes,
            skipped_kinds: Vec::new(),
            workspaces: reports,
        })
    }

    /// One workspace's contribution, with every failure mode folded into a note.
    ///
    /// Returns no `Result`: a registered checkout can be stale, moved, or owned
    /// by another machine, and that must degrade exactly one workspace rather
    /// than the query. The reported [`GlobalSearchMode`] is the sub-runtime's
    /// own — whether its vector branch ran — not a re-derivation from the hits
    /// that survived filtering, so a workspace whose hybrid hits were all
    /// hidden by a status filter still counts toward the fused `hybrid` mode
    /// [ORB-12259].
    fn query_one_workspace(
        &self,
        catalog: &dyn WorkspaceCatalog,
        target: &FederatedWorkspaceTarget,
        params: &GlobalSearchParams,
        query_model: Option<&str>,
        notes: &mut Vec<String>,
    ) -> (
        Vec<GlobalSearchHit>,
        WorkspaceSearchReport,
        GlobalSearchMode,
    ) {
        let mut report = WorkspaceSearchReport {
            workspace_id: target.workspace_id.clone(),
            name: target.name.clone(),
            hits: 0,
            note: None,
        };
        let mut record_note = |report: &mut WorkspaceSearchReport, note: String| {
            notes.push(workspace_note(&target.name, &note));
            report.note = Some(match report.note.take() {
                Some(existing) => format!("{existing}; {note}"),
                None => note,
            });
        };

        let runtime = match catalog.open(target) {
            Ok(runtime) => runtime,
            Err(error) => {
                record_note(&mut report, format!("skipped: {error}"));
                return (Vec::new(), report, GlobalSearchMode::Lexical);
            }
        };
        if let Some(note) = query_model.and_then(|model| model_mismatch_note(&runtime, model)) {
            record_note(&mut report, note);
        }

        // Scope is reset so the sub-runtime — which carries a catalog of its
        // own — takes the plain single-workspace path and cannot recurse.
        let mut scoped = params.clone();
        scoped.workspaces = WorkspaceScope::Current;
        match runtime.workspace_search(scoped) {
            Ok(response) => {
                for note in response.notes {
                    notes.push(workspace_note(&target.name, &note));
                }
                let mode = response.mode;
                let hits = response
                    .results
                    .into_iter()
                    .map(|hit| attribute(hit, target))
                    .collect::<Vec<_>>();
                report.hits = hits.len();
                (hits, report, mode)
            }
            Err(error) => {
                record_note(&mut report, format!("skipped: {error}"));
                (Vec::new(), report, GlobalSearchMode::Lexical)
            }
        }
    }
}

fn federation_unavailable() -> OrbitError {
    OrbitError::InvalidInput(
        "multi-workspace search needs a registry-backed runtime; this runtime is bound to a single checkout"
            .to_string(),
    )
}

/// Modes that only mean something inside one checkout.
///
/// Enforced here rather than in each surface: this is the domain rule, and the
/// CLI, the tool host, and the HTTP adapter all reach it through this call.
pub(super) fn ensure_federated_scope_supported(
    params: &GlobalSearchParams,
) -> Result<(), OrbitError> {
    if params.semantic.is_some() {
        return Err(OrbitError::InvalidInput(
            "`semantic` neighbor lookup is single-workspace; it ranks against one workspace's task vectors"
                .to_string(),
        ));
    }
    if params.path.is_some() {
        return Err(OrbitError::InvalidInput(
            "`path` applicability lookup is single-workspace; a checkout path belongs to one workspace"
                .to_string(),
        ));
    }
    Ok(())
}

/// The sandbox posture for a federated read from inside an agent run: denied.
///
/// The v2 host allow-lists only the run's own `{workspace}/state/semantic.db*`.
/// An in-process federated read would hand a run scoped to an unrelated code
/// workspace a handle on every registered index — including a personal vault —
/// which turns a filesystem-level guarantee into a query-time filter. Denying
/// the *scope* rather than widening the allow-list keeps the two consistent.
/// Human and operator surfaces, which are not inside a managed run, keep it.
pub(super) fn ensure_federated_scope_permitted(in_managed_run: bool) -> Result<(), OrbitError> {
    if in_managed_run {
        return Err(OrbitError::InvalidInput(
            "multi-workspace search is not available inside an Orbit-managed run; a run may only read its own workspace index"
                .to_string(),
        ));
    }
    Ok(())
}

pub(super) fn apply_workspace_cap(targets: &mut Vec<FederatedWorkspaceTarget>) -> Option<String> {
    if targets.len() <= MAX_FEDERATED_WORKSPACES {
        return None;
    }
    let dropped = targets.len() - MAX_FEDERATED_WORKSPACES;
    targets.truncate(MAX_FEDERATED_WORKSPACES);
    Some(format!(
        "workspace scope capped at {MAX_FEDERATED_WORKSPACES}; {dropped} further registered workspace(s) were not queried"
    ))
}

/// Whether this workspace's vectors can contribute cosine rank at all.
///
/// A workspace indexed under a different `model_id` has no rows the query
/// embedder can score, so its hybrid branch degrades to lexical. Say so by
/// name instead of emitting a plausible-looking ranking the caller cannot
/// tell apart from a fused one.
fn model_mismatch_note(runtime: &OrbitRuntime, query_model: &str) -> Option<String> {
    let indexed = runtime
        .stores()
        .semantic_index()
        .store()
        .ok()?
        .model_ids()
        .ok()?;
    describe_model_mismatch(&indexed, query_model)
}

pub(super) fn describe_model_mismatch(
    indexed: &BTreeSet<String>,
    query_model: &str,
) -> Option<String> {
    if indexed.is_empty() || indexed.contains(query_model) {
        return None;
    }
    let models = indexed.iter().cloned().collect::<Vec<_>>().join(", ");
    Some(format!(
        "semantic index uses model(s) {models}, not the query model {query_model}; cosine scores are not fused for this workspace and it contributes lexical hits only"
    ))
}

pub(super) fn workspace_note(name: &str, note: &str) -> String {
    format!("[{name}] {note}")
}

pub(super) fn attribute(
    mut hit: GlobalSearchHit,
    target: &FederatedWorkspaceTarget,
) -> GlobalSearchHit {
    hit.workspace = Some(HitWorkspace {
        workspace_id: target.workspace_id.clone(),
        name: target.name.clone(),
        repo_root: target.repo_root.to_string_lossy().into_owned(),
    });
    hit
}
