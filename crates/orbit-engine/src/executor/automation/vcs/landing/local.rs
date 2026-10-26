//! Landing a local candidate, an already-landed candidate and a no-diff handoff.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::DirectLandingRequest;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::super::input::input_string_field;
use super::super::git::{git_command_success, git_output};
use super::super::worktree::{checkout_holding_branch, ensure_clean_checkout};
use super::intent::LandingView;
use super::observe::{contains_commit, observe_or_stop, resolve_landing_ref};
use super::record::{complete, record, stop, stop_for_repair};
use super::run::LandingPass;

/// Owner-local delivery: no origin, no pull request, and a fast-forward of the
/// landing branch is the entire external effect.
pub(super) fn land_local_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    input: &Value,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let candidate = &context.candidate;
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(&workspace, &candidate.landing_branch, &candidate.delivery)?;

    if contains_commit(&workspace, &landing, &candidate.candidate.commit)? {
        let evidence = json!({
            "delivery": "local_candidate",
            "landing_ref": landing,
            "candidate": candidate.candidate.commit,
            "verified": "landing_ref_contains_candidate",
        });
        return complete(host, context, observed, &evidence);
    }

    let checkout = checkout_holding_branch(&workspace, &candidate.landing_branch)?
        .unwrap_or_else(|| workspace.clone());
    ensure_clean_checkout(&checkout, "owner landing checkout")?;
    let checked_out = git_output(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if checked_out.trim() != candidate.landing_branch {
        return Err(stop(
            host,
            context,
            &format!(
                "owner checkout '{}' has '{}' checked out, not landing branch '{}'",
                checkout.display(),
                checked_out.trim(),
                candidate.landing_branch
            ),
        )?);
    }

    // A fast-forward is a direct landing with no provider identity, so the
    // owner's delivery consumers learn which task it delivered only from an
    // intent retained before the branch moves [ORB-13894]. A candidate that
    // cannot fast-forward lands nothing and stops below.
    let before = git_output(&checkout, &["rev-parse", "HEAD"])?;
    let run_id = input_string_field(input, "run_id");
    if git_command_success(
        &checkout,
        &[
            "merge-base",
            "--is-ancestor",
            before.trim(),
            &candidate.candidate.commit,
        ],
    )? {
        let run_id = run_id.clone().ok_or_else(|| {
            OrbitError::InvalidInput("handoff_land: run_id is required".to_string())
        })?;
        host.record_direct_landing_intent(&DirectLandingRequest {
            run_id,
            branch: candidate.landing_branch.clone(),
            before_commit: before.trim().to_string(),
            after_commit: candidate.candidate.commit.clone(),
            task_ids: vec![context.task_id.clone()],
            handoff_id: Some(context.handoff_id.clone()),
        })?;
    }

    let intent_id = format!("{}:local", context.handoff_id);
    record(
        host,
        context,
        HandoffLandingStep::PublishIntent {
            intent_id: intent_id.clone(),
        },
        Some(observed.clone()),
        &json!({
            "sending": "local_fast_forward",
            "landing_branch": candidate.landing_branch,
            "candidate": candidate.candidate.commit,
        }),
    )?;
    let fast_forwarded = git_command_success(
        &checkout,
        &["merge", "--ff-only", &candidate.candidate.commit],
    )?;
    // The ref itself is the evidence either way: a failed fast-forward left it
    // where it was, which is a resolved intent rather than an uncertain one.
    let merged = contains_commit(&workspace, &landing, &candidate.candidate.commit)?;
    let evidence = json!({
        "delivery": "local_candidate",
        "landing_ref": landing,
        "candidate": candidate.candidate.commit,
        "fast_forwarded": fast_forwarded,
        "verified": if merged { "landing_ref_contains_candidate" } else { "landing_ref_unchanged" },
    });
    record(
        host,
        context,
        HandoffLandingStep::ResolveIntent { intent_id, merged },
        Some(observed.clone()),
        &evidence,
    )?;
    if !merged {
        return Err(stop_for_repair(
            host,
            context,
            &format!(
                "landing branch '{}' cannot fast-forward to candidate {}; the owner does not \
                 rebase unvalidated code, so a repair needs fresh validation and a new handoff",
                candidate.landing_branch, candidate.candidate.commit
            ),
        )?);
    }
    // The branch has moved: a failure to record the landing's diff stats is
    // logged and never stops the completion.
    if let Some(run_id) = run_id
        && before.trim() != candidate.candidate.commit
        && let Err(error) = host.record_direct_landing(&DirectLandingRequest {
            run_id,
            branch: candidate.landing_branch.clone(),
            before_commit: before.trim().to_string(),
            after_commit: candidate.candidate.commit.clone(),
            task_ids: vec![context.task_id.clone()],
            handoff_id: Some(context.handoff_id.clone()),
        })
    {
        tracing::warn!(%error, task_id = %context.task_id, "recording the landing's diff stats failed");
    }
    complete(host, context, observed, &evidence)
}

/// No-diff delivery: the work is already on the landing branch, so there is
/// nothing to merge. The typed evidence and completion authority are still
/// required — the owner store rechecks both — and the covering commit is
/// verified here against the real landing ref.
pub(super) fn land_already_landed<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    covering_commit: &str,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let workspace = context.workspace_path.clone();
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(
        &workspace,
        &context.candidate.landing_branch,
        &context.candidate.delivery,
    )?;
    if !contains_commit(&workspace, &landing, covering_commit)? {
        return Err(stop(
            host,
            context,
            &format!(
                "covering commit {covering_commit} is not contained in landing ref \
                 '{landing}'{}, so this delivery is not already landed",
                view.clause()
            ),
        )?);
    }
    let evidence = json!({
        "delivery": "already_landed",
        "landing_ref": landing,
        "landing_ref_refreshed": view.refreshed,
        "covering_commit": covering_commit,
        "verified": "landing_ref_contains_covering_commit",
        "external_merge": false,
    });
    complete(host, context, observed, &evidence)
}

/// A verified clean base completes without an external merge. The Core host
/// rechecks the pinned verifier report, and that the live base still contains
/// the synchronized base, at the completion write. The landing branch may
/// have advanced past that base since the run synchronized [ORB-15074];
/// `observe_or_stop` already stops when it no longer contains it.
pub(super) fn land_no_diff<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    view: &LandingView,
) -> Result<Value, OrbitError> {
    let observed = observe_or_stop(host, context, None, view)?;
    let landing = resolve_landing_ref(
        &context.workspace_path,
        &observed.landing_branch,
        &observed.delivery,
    )?;
    if observed.candidate != observed.base {
        return Err(stop(
            host,
            context,
            "NoDiff candidate is not its synchronized base; revalidate the current base",
        )?);
    }
    complete(
        host,
        context,
        observed,
        &json!({
            "delivery": "no_diff", "landing_ref": landing,
            "verified": "clean_base_checkpoint", "external_merge": false,
        }),
    )
}
