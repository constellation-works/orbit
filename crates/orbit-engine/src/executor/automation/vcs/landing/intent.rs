//! Reconciling a recorded merge intent against what the provider or target ref shows.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::handoff::HandoffDelivery;
use serde_json::{Value, json};

use crate::context::{HandoffLandingStep, RuntimeHost};

use super::super::git::{fetch_remote_base, git_command_success};
use super::super::pr::diagnostics;
use super::super::pr::{PrMergeState, classify_pr_state, read_pr_status};
use super::observe::{
    adopt_base_update, contains_commit, observe_candidate, pin_for, resolve_landing_ref,
};
use super::record::{complete, describe, record, stop, stop_pr};
use super::run::LandingPass;

/// The owner's view of the landing branch, refreshed once per attempt.
///
/// Containment is judged against `origin/<landing branch>`, a remote-tracking
/// ref that only moves when this checkout fetches it. A merge another consumer
/// performed on the provider, or a covering commit another machine landed,
/// leaves that ref behind the real branch — and judging against it unrefreshed
/// turns ordinary freshness into a durable refusal whose evidence says the
/// opposite of what is true [ORB-12631]. Fetching the branch also brings the
/// objects reachable from it, which is how a base or covering commit the owner
/// has never seen becomes readable here.
pub(super) struct LandingView {
    pub(super) refreshed: bool,
}

impl LandingView {
    /// Fetch the landing branch from `origin`, best effort.
    ///
    /// Owner-local delivery has no remote to read, and a fetch that cannot run
    /// (no `origin`, no network) leaves the judgement to the refs already
    /// present rather than replacing a freshness problem with a different
    /// failure: what the owner still cannot verify is refused below either way.
    /// Adopting a published PR's updated head requires its own successful
    /// fetch, even when this initial refresh failed.
    pub(super) fn refresh(
        workspace: &Path,
        landing_branch: &str,
        delivery: &HandoffDelivery,
    ) -> Self {
        if matches!(delivery, HandoffDelivery::LocalCandidate)
            || !git_command_success(workspace, &["remote", "get-url", "origin"]).unwrap_or(false)
        {
            return Self { refreshed: false };
        }
        match fetch_remote_base(workspace, landing_branch) {
            Ok(()) => Self { refreshed: true },
            Err(error) => {
                tracing::warn!(
                    %error,
                    landing_branch,
                    "handoff_land: landing ref not refreshed; containment is judged against the \
                     owner's existing view"
                );
                Self { refreshed: false }
            }
        }
    }

    /// Names the state a refusal was decided against, so an operator reading
    /// "no longer reachable" knows whether the owner's view was current.
    pub(super) fn clause(&self) -> &'static str {
        if self.refreshed {
            " (refreshed from 'origin')"
        } else {
            ""
        }
    }
}

/// Read what actually became of an unresolved merge intent and record it.
///
/// Returns the completion outcome when the external state proves the candidate
/// merged; `None` means the intent is resolved as *not* merged and this run may
/// attempt a fresh, fully rechecked landing. A merged pull request with a
/// different identity resolves the external uncertainty before recording Stop;
/// resolving an intent is not proof that the authorized candidate landed.
pub(super) fn reconcile_intent<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &LandingPass,
    intent_id: &str,
    view: &LandingView,
) -> Result<Option<Value>, OrbitError> {
    let candidate = &context.candidate;
    let mut delivery_error = None;
    let mut refusal_observation = None;
    let (merged, evidence) = match &candidate.delivery {
        HandoffDelivery::PullRequest { number } => {
            let status = read_pr_status(host, &context.workspace_path, &number.to_string())?;
            refusal_observation = Some(json!({
                "pr":diagnostics::status(&status), "status_observations":1,
                "status_history_truncated":false,
                "required_checks":{"state":"not_collected"},
                "unreported_wait":{"state":"not_collected"},
                "classification":diagnostics::text(&describe(&classify_pr_state(&status))),
            }));
            match classify_pr_state(&status) {
                PrMergeState::Merged => {
                    let mut pin = pin_for(candidate);
                    adopt_base_update(&mut pin, context, &status);
                    let evidence = match pin.ensure_delivered(&status, &number.to_string()) {
                        Ok(delivered) => json!({
                            "reconciled": "pull_request_merged",
                            "pull_request": number,
                            "delivery": delivered.as_json(),
                        }),
                        Err(error) => {
                            let evidence = json!({
                                "reconciled": "pull_request_merged",
                                "pull_request": number,
                                "provider_state": diagnostics::status(&status),
                                "delivery_error": diagnostics::reason(&error.to_string()),
                            });
                            delivery_error = Some(error);
                            evidence
                        }
                    };
                    (true, evidence)
                }
                other => (
                    false,
                    json!({
                        "reconciled": "pull_request_open",
                        "pull_request": number,
                        "provider_state": diagnostics::text(&describe(&other)),
                    }),
                ),
            }
        }
        HandoffDelivery::LocalCandidate => {
            let landing = resolve_landing_ref(
                &context.workspace_path,
                &candidate.landing_branch,
                &candidate.delivery,
            )?;
            let merged = contains_commit(
                &context.workspace_path,
                &landing,
                &candidate.candidate.commit,
            )?;
            (
                merged,
                json!({
                    "reconciled": if merged { "local_target_contains_candidate" } else { "local_target_unchanged" },
                    "landing_ref": landing,
                    "candidate": candidate.candidate.commit,
                }),
            )
        }
        // No-diff delivery performs no external call, so it never publishes an
        // intent; an intent recorded against one is not something this activity
        // may resolve by guessing.
        HandoffDelivery::AlreadyLanded { .. } | HandoffDelivery::NoDiff { .. } => {
            return Err(OrbitError::Execution(format!(
                "handoff_land: handoff '{}' records an unresolved merge intent for no-diff \
                 delivery, which performs no external merge; reconcile it explicitly",
                context.handoff_id
            )));
        }
    };

    // Resolving records what was read, not an authority decision, so it needs
    // no candidate observation — and clearing the uncertainty first is what
    // lets a stop or a fresh attempt happen at all.
    record(
        host,
        context,
        HandoffLandingStep::ResolveIntent {
            intent_id: intent_id.to_string(),
            merged,
        },
        None,
        &evidence,
    )?;
    if let Some(error) = delivery_error {
        return Err(stop_pr(
            host,
            context,
            refusal_observation.as_ref().unwrap_or(&Value::Null),
            false,
            &error.to_string(),
        )?);
    }
    if !merged {
        return Ok(None);
    }
    let observed = observe_candidate(context, None, view).map_err(|error| {
        if let Some(observation) = &refusal_observation {
            stop_pr(host, context, observation, false, &error.to_string()).unwrap_or(error)
        } else {
            stop(host, context, &error.to_string()).unwrap_or(error)
        }
    })?;
    Ok(Some(complete(host, context, observed, &evidence)?))
}
