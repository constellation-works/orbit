//! Can a delivery against this base branch still reach the branch work lands
//! on?
//!
//! A resumed PR pipeline used to report success against a base that had
//! already merged and been deleted (ORB-10644). Every step passed — the base
//! name flows `input.base_branch -> prepare_branch -> sync_base -> pr_open`
//! and was never re-derived — because `resolve_worktree_start_point` is
//! satisfied by any `origin/<base>` that resolves, and a leftover or restored
//! branch resolves to its pre-merge tip. The PR then opened against a branch
//! nothing merges any more, so the commit never landed while every signal said
//! it had.
//!
//! # What "obsolete" means here
//!
//! A base is obsolete when it can no longer carry work to the landing branch,
//! by either of two tests:
//!
//! 1. **Deleted.** The repository has an `origin` remote and the base branch no
//!    longer exists on it. A PR cannot merge into a branch the remote does not
//!    have, so a local (or stale remote-tracking) ref that still resolves is a
//!    leftover, not a target.
//! 2. **Already landed.** A `landing_branch` was declared, differs from the
//!    base, and the base carries nothing the landing branch does not already
//!    have: either the base tip is a strict ancestor of the landing branch
//!    (merge / fast-forward), or every commit unique to the base is already
//!    delivered on the landing branch under its task marker (squash / rebase —
//!    the shape Orbit's own `merge_batch_pr` produces, where the sha is rewritten
//!    and only the marker survives).
//!    Distinct branches at the same tip stay live: a fresh stacked base has
//!    no work of its own to have landed yet and must accept its first delivery.
//!
//! Test 2 deliberately reuses `delivery_marker`, the same reasoning
//! `worktree::dependency_delivery` uses for "is this actually merged into
//! base", rather than a parallel rule.
//!
//! # False positives
//!
//! Test 2 flags a live base whose only unique commits repeat a task id that
//! already landed (a task re-opened and re-run, or work folded into a sibling
//! task under the same id). Test 1 flags a base kept deliberately local while
//! an `origin` remote exists. Both are refusals of work that could have
//! shipped; both are escapable with `input.base_obsolescence='ignore'`. The
//! trade is deliberate — the failure this replaces is silent, and a loud
//! refusal is recoverable in a way a "successful" non-delivery is not.
//!
//! The gate stays inert for ordinary delivery: with no `landing_branch`
//! declared, or with the landing branch equal to the base (the overwhelmingly
//! common non-stacked case), only the cheap remote-existence probe runs.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use serde_json::Value;

use super::super::input::input_string_field;
use super::delivery_marker::{delivery_markers, marker_reachable};
use super::freshness::{commit_sha, remote_branch_sha};
use super::git::{
    BaseSyncMode, base_sync_mode_from_input, git_command_success, git_output_raw,
    normalize_base_branch, resolve_worktree_start_point,
};

/// How much unique base history the squash-landing test will read. A stacked
/// base carries a handful of commits; anything longer is a long-lived
/// integration branch, which this test has no business declaring landed.
const MAX_INSPECTED_BASE_COMMITS: usize = 25;

/// Whether delivery enforces base obsolescence.
///
/// `Enforce` is the default. `Ignore` exists for the cases the two tests
/// cannot see: a base deliberately kept off `origin`, or a live base whose
/// commits repeat an already-landed task id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BaseObsolescenceMode {
    Enforce,
    Ignore,
}

fn base_obsolescence_mode_from_input(input: &Value) -> Result<BaseObsolescenceMode, OrbitError> {
    match input
        .as_object()
        .and_then(|map| map.get("base_obsolescence"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("enforce") => Ok(BaseObsolescenceMode::Enforce),
        Some("ignore") => Ok(BaseObsolescenceMode::Ignore),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "input.base_obsolescence must be 'enforce' or 'ignore', got '{other}'"
        ))),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BaseStatus {
    /// Work merged into this base can still reach the landing branch.
    Live,
    /// The base branch is gone from `origin`.
    Deleted,
    /// The base carries nothing the landing branch does not already have.
    Landed(String),
}

/// Refuse `phase` when `base` can no longer carry this delivery.
///
/// `base_sha` is the commit the run pinned for the base (ADR-0251, L-0113),
/// never a moving name: the question is whether *the base this run was cut
/// from* has already landed. Only the landing branch is resolved live, because
/// its current tip is exactly what the question is about.
pub(in crate::executor::automation) fn ensure_base_can_still_land(
    repo_root: &Path,
    phase: &str,
    base: &str,
    base_sha: &str,
    input: &Value,
) -> Result<(), OrbitError> {
    if base_obsolescence_mode_from_input(input)? == BaseObsolescenceMode::Ignore {
        return Ok(());
    }
    let landing = input_string_field(input, "landing_branch")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let status = classify_base(
        repo_root,
        base,
        base_sha,
        landing.as_deref(),
        base_sync_mode_from_input(input)?,
    )?;
    match status {
        BaseStatus::Live => Ok(()),
        BaseStatus::Deleted => Err(obsolete_base_error(
            phase,
            base,
            landing.as_deref(),
            "it no longer exists on 'origin', so nothing merged into it can reach the branch this work lands on",
        )),
        BaseStatus::Landed(detail) => Err(obsolete_base_error(
            phase,
            base,
            landing.as_deref(),
            &format!("it has already landed — {detail}"),
        )),
    }
}

fn classify_base(
    repo_root: &Path,
    base: &str,
    base_sha: &str,
    landing_branch: Option<&str>,
    sync_mode: BaseSyncMode,
) -> Result<BaseStatus, OrbitError> {
    let base = normalize_base_branch(base)?;
    if has_origin_remote(repo_root)? && remote_branch_sha(repo_root, &base)?.is_none() {
        return Ok(BaseStatus::Deleted);
    }

    let Some(landing_branch) = landing_branch else {
        return Ok(BaseStatus::Live);
    };
    let landing = normalize_base_branch(landing_branch)?;
    if landing == base {
        return Ok(BaseStatus::Live);
    }

    let landing_ref = resolve_worktree_start_point(repo_root, &landing, sync_mode)?;
    let landing_sha = commit_sha(repo_root, &landing_ref)?;
    if base_sha == landing_sha {
        return Ok(BaseStatus::Live);
    }
    if git_command_success(
        repo_root,
        &["merge-base", "--is-ancestor", base_sha, &landing_sha],
    )? {
        return Ok(BaseStatus::Landed(format!(
            "its tip '{base_sha}' is already an ancestor of '{landing_ref}' ('{landing_sha}')"
        )));
    }

    Ok(
        match squash_landed_markers(repo_root, base_sha, &landing_sha)? {
            Some(markers) => BaseStatus::Landed(format!(
                "every commit it carries beyond '{landing_ref}' is already delivered there under {}",
                markers.join(", ")
            )),
            None => BaseStatus::Live,
        },
    )
}

/// `Some(markers)` when every commit unique to `base_sha` is already delivered
/// on `landing_sha` under a task marker — the squash-merge shape, where the
/// base tip is not an ancestor of the landing branch but its work is there.
///
/// `None` (base still live) whenever the answer is not unanimous: one commit
/// with no marker, or one marker that has not reached the landing branch, is
/// enough to mean the base still carries undelivered work.
fn squash_landed_markers(
    repo_root: &Path,
    base_sha: &str,
    landing_sha: &str,
) -> Result<Option<Vec<String>>, OrbitError> {
    let raw = git_output_raw(
        repo_root,
        &[
            "log",
            "--no-color",
            "--format=%B%x00",
            &format!("--max-count={}", MAX_INSPECTED_BASE_COMMITS + 1),
            &format!("{landing_sha}..{base_sha}"),
        ],
    )?;
    let messages = raw
        .split('\0')
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .collect::<Vec<_>>();
    if messages.is_empty() || messages.len() > MAX_INSPECTED_BASE_COMMITS {
        return Ok(None);
    }

    let mut reachable: BTreeMap<String, bool> = BTreeMap::new();
    let mut landed = Vec::new();
    for message in messages {
        let mut delivered = None;
        for marker in delivery_markers(message) {
            let is_reachable = match reachable.get(&marker) {
                Some(known) => *known,
                None => {
                    let known = marker_reachable(repo_root, &marker, landing_sha)?;
                    reachable.insert(marker.clone(), known);
                    known
                }
            };
            if is_reachable {
                delivered = Some(marker);
                break;
            }
        }
        match delivered {
            Some(marker) => {
                if !landed.contains(&marker) {
                    landed.push(marker);
                }
            }
            None => return Ok(None),
        }
    }
    Ok(Some(landed))
}

fn has_origin_remote(repo_root: &Path) -> Result<bool, OrbitError> {
    git_command_success(repo_root, &["remote", "get-url", "origin"])
}

fn obsolete_base_error(
    phase: &str,
    base: &str,
    landing_branch: Option<&str>,
    reason: &str,
) -> OrbitError {
    let landing_clause = landing_branch
        .map(|landing| format!(" (landing branch '{landing}')"))
        .unwrap_or_default();
    let recovery = match landing_branch {
        Some(landing) => format!(
            "re-dispatch this run with base '{landing}', or restore '{base}' and land it first"
        ),
        None => format!(
            "re-dispatch this run against the branch this work lands on, or restore '{base}' on 'origin' and land it first"
        ),
    };
    OrbitError::Execution(format!(
        "{phase}: refusing delivery against obsolete base branch '{base}'{landing_clause}: {reason}. \
         Recovery: {recovery}; pass input.base_obsolescence='ignore' only when this base is deliberately still open."
    ))
}
