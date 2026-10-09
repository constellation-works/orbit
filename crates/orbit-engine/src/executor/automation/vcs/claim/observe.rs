use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;
use orbit_types::workflow::handoff::{HandoffCandidate, HandoffDelivery};
use serde_json::Value;

use crate::context::ClaimExecutionContext;
use crate::executor::automation::input::input_string_field;

use super::super::git::{
    BaseSyncMode, GitTimeoutBudget, GitTimeoutBudgetGuard, git_command_success, git_output,
    normalize_base_branch, resolve_worktree_start_point,
};
use super::super::review_gate::revision;
use super::delivery::repository;
use super::input::refused;
use super::{delivery, require_clean_checkout};

/// Observe a candidate and the base it sits on, in one checkout.
///
/// This is the single observation rule. The executor runs it on its own
/// worktree to build the handoff; the owner runs it again on its checkout to
/// decide whether the handoff describes anything real. Because both sides
/// derive the same fields the same way, an owner that disagrees is reporting a
/// genuine difference rather than a second implementation's quirk.
///
/// `source` names the branch to read, or `None` for whatever is checked out.
/// `fallback_repository` is used when the checkout has no remote to name.
/// `base_sync` is the run's sync mode (`local` or `remote`): the base *ref*
/// is resolved through `resolve_worktree_start_point`, the same mapping
/// every other step of the claimed pipeline uses, so a remote-sync claim
/// still fetches `origin/<base>` rather than a lagging local
/// `refs/heads/<base>`. The recorded base is then the merge-base of the
/// candidate and that ref — the tip the candidate sits on — not the live
/// fetched tip. A later advance of `origin/<base>` is therefore not a
/// refusal of a candidate that was synchronized onto the earlier SHA.
/// If a remote fetch exhausts its transport retries, a delivered candidate
/// may use the cached remote ref when its merge-base still resolves. Missing
/// or unrelated cached refs preserve the transient fetch failure. Refusals
/// and NoDiff observations always require a successful fetch.
pub fn observe_candidate(
    workspace_path: &Path,
    source: Option<&str>,
    base_branch: &str,
    landing_branch: &str,
    delivery: HandoffDelivery,
    fallback_repository: &str,
    base_sync: &str,
) -> Result<HandoffCandidate, OrbitError> {
    let source_branch = match source {
        Some(branch) => branch.to_string(),
        None => git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?,
    };
    if source_branch.trim().is_empty() || source_branch == "HEAD" {
        // A detached HEAD has no name the owner could resolve in its own
        // checkout, so the handoff would pin a branch that means something
        // different on each side.
        return Err(refused(
            "a claimed candidate must sit on a named branch; this checkout has a detached HEAD",
        ));
    }
    let candidate = revision(workspace_path, &source_branch)?;
    let sync_mode = match base_sync.trim() {
        "local" => BaseSyncMode::Local,
        "remote" => BaseSyncMode::Remote,
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "base_sync must be 'local' or 'remote', got '{other}'"
            )));
        }
    };
    let base_ref = match resolve_worktree_start_point(workspace_path, base_branch, sync_mode) {
        Ok(base_ref) => base_ref,
        Err(error)
            if sync_mode == BaseSyncMode::Remote
                && !matches!(delivery, HandoffDelivery::NoDiff { .. })
                && matches!(&error, OrbitError::Execution(message) | OrbitError::ExecutionTimeout { message, .. } if message.starts_with(TRANSIENT_FAILURE_MARKER)) =>
        {
            let cached = format!("origin/{}", normalize_base_branch(base_branch)?);
            if !git_command_success(workspace_path, &["merge-base", &candidate.commit, &cached])
                .unwrap_or(false)
            {
                return Err(error);
            }
            tracing::warn!(base_ref = cached, %error, "observing claimed candidate against cached remote base");
            cached
        }
        Err(error) => return Err(error),
    };
    let tip = revision(workspace_path, &base_ref)?;
    // Same rule as `synchronized_base`: the candidate is judged against the
    // merge-base it sits on, not against a tip that can move under a fetch.
    if !git_command_success(
        workspace_path,
        &["merge-base", &candidate.commit, &base_ref],
    )? {
        return Err(refused(format!(
            "candidate '{}' does not descend from validated base '{}'",
            candidate.commit, tip.commit
        )));
    }
    let merge_base = git_output(
        workspace_path,
        &["merge-base", &candidate.commit, &base_ref],
    )?;
    let base = revision(workspace_path, &merge_base)?;
    if let HandoffDelivery::NoDiff { .. } = delivery {
        if candidate != tip {
            return Err(refused(
                "a NoDiff handoff must validate the current base itself",
            ));
        }
    } else if base.commit == candidate.commit {
        return Err(refused(
            "the candidate is the base itself; a claimed leaf hands off delivered work, and \
             no-diff delivery is not part of this route",
        ));
    }
    let ancestor = git_command_success(
        workspace_path,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            &base.commit,
            &candidate.commit,
        ],
    )?;
    if !ancestor {
        return Err(refused(format!(
            "candidate '{}' does not descend from validated base '{}'",
            candidate.commit, base.commit
        )));
    }
    let landing_branch = if landing_branch.trim().is_empty() {
        base_branch
    } else {
        landing_branch
    };
    Ok(HandoffCandidate {
        repository: repository(workspace_path, fallback_repository),
        source_branch,
        base_branch: base_branch.to_string(),
        landing_branch: landing_branch.to_string(),
        candidate,
        base,
        delivery,
    })
}

/// The executor's own observation: whatever is checked out, against the base
/// this claim was synchronized onto. A `base_sha` carried from `sync_base`
/// must be contained in the candidate (an ancestor), never compared to the
/// live `origin/<base>` tip: a later advance of that tip is not a refusal.
pub(super) fn observe(
    workspace_path: &Path,
    context: &ClaimExecutionContext,
    input: &Value,
) -> Result<HandoffCandidate, OrbitError> {
    observe_with(workspace_path, context, input, delivery(context, input)?)
}

/// [`observe`] with an explicit delivery.
pub(super) fn observe_with(
    workspace_path: &Path,
    context: &ClaimExecutionContext,
    input: &Value,
    delivery: HandoffDelivery,
) -> Result<HandoffCandidate, OrbitError> {
    let _timeout_budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget::from_input(input)?);
    let candidate = observe_candidate(
        workspace_path,
        None,
        &context.base_branch,
        &context.landing_branch,
        delivery,
        &context.workspace_id,
        &claimed_base_sync(context, input)?,
    )?;
    if let Some(declared) = input_string_field(input, "base_sha") {
        let contains_declared = git_command_success(
            workspace_path,
            &[
                "merge-base",
                "--is-ancestor",
                "--end-of-options",
                &declared,
                &candidate.candidate.commit,
            ],
        )?;
        if !contains_declared {
            return Err(refused(format!(
                "candidate '{}' does not descend from validated base '{declared}'",
                candidate.candidate.commit
            )));
        }
    }
    Ok(candidate)
}

/// The claimed candidate must still be the clean checkout it was observed as.
pub(super) fn require_clean_candidate(
    workspace_path: &Path,
    candidate: &HandoffCandidate,
) -> Result<(), OrbitError> {
    require_clean_checkout(
        workspace_path,
        &candidate.source_branch,
        &candidate.candidate.commit,
        "the claimed candidate",
    )
}

/// The run's sync mode, which every other claimed-leaf step already honors.
///
/// An explicit `base_sync` on the activity input wins. When it is absent, ship
/// mode is the durable proxy the admission transaction used to pin the run
/// (`local` → local ref, `pr` → `origin/<base>`). That fallback is required:
/// `base_sync_mode_from_input` treats a missing field as remote, which would
/// send the owner-local route looking for an origin it does not have.
pub(super) fn claimed_base_sync(
    context: &ClaimExecutionContext,
    input: &Value,
) -> Result<String, OrbitError> {
    if let Some(value) = input_string_field(input, "base_sync") {
        return Ok(value);
    }
    match context.ship_mode.as_str() {
        "local" => Ok("local".to_string()),
        "pr" => Ok("remote".to_string()),
        other => Err(refused(format!(
            "claimed leaf ship mode '{other}' has no base sync mode"
        ))),
    }
}
