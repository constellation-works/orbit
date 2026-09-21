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
//!   and `None` (frictions) are not commensurable
//!   across workspaces, so nothing compares them, and no single large
//!   workspace can crowd out the rest.
//! * **Every hit is attributed.** Friction and job-run IDs are allocated per
//!   workspace, so a merged list without a workspace field lets a caller route
//!   a follow-up write to the wrong record (F2026-08-046).

use std::sync::atomic::{AtomicUsize, Ordering};

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

/// How many of those checkouts are queried at once.
///
/// Serially, a fan-out costs the sum of every workspace's runtime open, index
/// read, and query; the per-workspace work is independent and merged by rank
/// afterwards, so it runs on a pool instead. The pool is bounded rather than
/// one thread per target because each in-flight query holds a full runtime —
/// every SQLite store plus a lexical index — and that footprint, not CPU, is
/// what the bound protects [DANI-10365].
pub(super) const MAX_FEDERATED_CONCURRENCY: usize = 8;

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

        let outcomes = fan_out(catalog.as_ref(), &targets, &params);

        let mut branches = Vec::with_capacity(outcomes.len());
        let mut reports = Vec::with_capacity(outcomes.len());
        for outcome in outcomes {
            notes.extend(outcome.notes);
            branches.push(outcome.hits);
            reports.push(outcome.report);
        }

        let results = merge_round_robin(branches, params.normalized_limit());
        Ok(GlobalSearchResponse {
            mode: GlobalSearchMode::Lexical,
            kind: params.kind,
            results,
            notes,
            skipped_kinds: Vec::new(),
            workspaces: reports,
        })
    }
}

/// What one workspace contributed to the fused answer.
///
/// `notes` is carried per workspace rather than appended to one shared buffer
/// so the concurrent fan-out still emits notes in target order — identical to
/// the serial order a reader and the existing tests expect.
struct WorkspaceOutcome {
    hits: Vec<GlobalSearchHit>,
    report: WorkspaceSearchReport,
    notes: Vec<String>,
}

/// Query every target over a bounded pool, in target order.
///
/// Workers pull the next unclaimed target rather than taking a fixed slice, so
/// one slow checkout — a cold SQLite page cache, a network mount — does not
/// idle the pool behind it. Results are re-sorted by target index, so ordering
/// and attribution do not depend on completion order.
fn fan_out(
    catalog: &dyn WorkspaceCatalog,
    targets: &[FederatedWorkspaceTarget],
    params: &GlobalSearchParams,
) -> Vec<WorkspaceOutcome> {
    let workers = federated_worker_count(targets.len());
    if workers <= 1 {
        return targets
            .iter()
            .map(|target| query_one_workspace(catalog, target, params))
            .collect();
    }

    let next = AtomicUsize::new(0);
    let claim_and_query = || {
        let mut claimed = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(target) = targets.get(index) else {
                return claimed;
            };
            claimed.push((index, query_one_workspace(catalog, target, params)));
        }
    };

    let mut collected = std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| scope.spawn(claim_and_query))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
            })
            .collect::<Vec<_>>()
    });
    collected.sort_by_key(|(index, _)| *index);
    collected.into_iter().map(|(_, outcome)| outcome).collect()
}

/// The pool width for a scope of `targets` workspaces: never wider than the
/// scope, never wider than [`MAX_FEDERATED_CONCURRENCY`].
pub(super) fn federated_worker_count(targets: usize) -> usize {
    targets.min(MAX_FEDERATED_CONCURRENCY)
}

/// One workspace's contribution, with failures folded into a note.
fn query_one_workspace(
    catalog: &dyn WorkspaceCatalog,
    target: &FederatedWorkspaceTarget,
    params: &GlobalSearchParams,
) -> WorkspaceOutcome {
    let mut report = WorkspaceSearchReport {
        workspace_id: target.workspace_id.clone(),
        name: target.name.clone(),
        hits: 0,
        note: None,
    };
    let mut notes = Vec::new();
    let record_note =
        |report: &mut WorkspaceSearchReport, notes: &mut Vec<String>, note: String| {
            notes.push(workspace_note(&target.name, &note));
            report.note = Some(match report.note.take() {
                Some(existing) => format!("{existing}; {note}"),
                None => note,
            });
        };

    let runtime = match catalog.open(target) {
        Ok(runtime) => runtime,
        Err(error) => {
            record_note(&mut report, &mut notes, format!("skipped: {error}"));
            return WorkspaceOutcome {
                hits: Vec::new(),
                report,
                notes,
            };
        }
    };
    // Scope is reset so the sub-runtime — which carries a catalog of its
    // own — takes the plain single-workspace path and cannot recurse.
    let mut scoped = params.clone();
    scoped.workspaces = WorkspaceScope::Current;
    match runtime.workspace_search(scoped) {
        Ok(response) => {
            for note in response.notes {
                notes.push(workspace_note(&target.name, &note));
            }
            let hits = response
                .results
                .into_iter()
                .map(|hit| attribute(hit, target))
                .collect::<Vec<_>>();
            report.hits = hits.len();
            WorkspaceOutcome {
                hits,
                report,
                notes,
            }
        }
        Err(error) => {
            record_note(&mut report, &mut notes, format!("skipped: {error}"));
            WorkspaceOutcome {
                hits: Vec::new(),
                report,
                notes,
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
