use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_engine::{BaseSyncMode, KnownRedUnconfirmed};
use orbit_store::contracts::{
    ClaimEvidence, ClaimFailure, ClaimInvocation, ClaimMutation, ExecutionClaim,
    HandoffObservation, KnownRedObservation, KnownRedOsAcceptance,
};
use orbit_types::workflow::ClaimFailureClass;
use orbit_types::workflow::handoff::{HandoffValidationLog, TaskHandoff};

use crate::OrbitRuntime;

use super::{refusal, settlement};

/// The code leading the reason a claim ends with when the owner's own base
/// run contradicts its leaf's known-red report [ORB-15566].
const KNOWN_RED_CONTRADICTED: &str = "known_red_contradicted";

/// The owner's judgement of a handoff's known-red logs: one observation per
/// log for the claim journal, and the verdicts its own base runs contradict.
#[derive(Default)]
pub(super) struct OwnerKnownRed {
    pub(super) observations: Vec<KnownRedObservation>,
    pub(super) contradicted: Vec<String>,
}

impl OrbitRuntime {
    /// [ORB-15566] End a claim whose known-red report the owner's own base run
    /// contradicts, and answer the settlement with the refusal.
    ///
    /// The handoff can never be accepted: retrying reads the same base result.
    /// Refusing it while holding the claim would leave the follower's
    /// immutable settlement pending until an operator revoked the claim. The
    /// owner releases the claim itself instead, as a `transient` failure
    /// counted against the task's release budget, so pull admission offers
    /// the task again and a third contradiction in the window blocks it. The
    /// follower reconciles the refusal against the ended claim and closes its
    /// settlement obsolete. Only the owner's own verdict ends the claim; the
    /// leaf's log is never accepted.
    pub(super) fn release_contradicted_known_red(
        &self,
        claim: &ExecutionClaim,
        context: &ClaimInvocation,
        contradicted: &[String],
    ) -> OrbitError {
        let verdict = format!(
            "{KNOWN_RED_CONTRADICTED}: the owner's own base run contradicts the leaf's known-red \
             report: {}",
            contradicted.join("; ")
        );
        let evidence = ClaimEvidence {
            summary: Some(format!("released by owner: {verdict}")),
            comment: Some(format!(
                "This owner released the claim because {verdict}. The handoff could never be \
                 accepted, so the task is back in the backlog and can be pulled again."
            )),
            failure: Some(ClaimFailure {
                class: ClaimFailureClass::Transient,
                review_decision_pending: false,
                reason: verdict.clone(),
                crew: None,
                candidate: None,
                provider_limit: false,
                provider_refusal: false,
                owner_busy: false,
            }),
            ..ClaimEvidence::default()
        };
        if let Err(error) = self.mutate_execution_claim(
            Some(context),
            &settlement::release_mutation_id(&claim.claim_id),
            &ClaimMutation::Release(evidence),
        ) {
            tracing::warn!(
                task_id = %claim.task_id,
                claim_id = %claim.claim_id,
                %verdict,
                "could not release a claim whose known-red report the owner contradicts: {error}"
            );
            return error;
        }
        refusal::refused(format!(
            "{verdict}; this owner released the claim, so the settlement is obsolete"
        ))
    }

    /// The owner's own judgement of each validation log a handoff reports
    /// as known red [ORB-15427].
    ///
    /// The leaf's base run is its claim, not evidence. For each pinned log
    /// carrying a known-red report, the owner runs the command on the base it
    /// observed the candidate on — or reads its own cached run — and keeps
    /// the reported failures only when that base is in the history of the
    /// base branch this owner captured at admission and its run fails every
    /// one ([`orbit_engine::verify_claimed_known_red`]). Only commands this
    /// owner requires are run; the log names one, never supplies it. A log
    /// that cannot be read is left to the claim journal, which refuses it.
    /// A verdict the owner's base run contradicts is also returned on its own
    /// [ORB-15566], since it ends the claim rather than refusing it.
    ///
    /// That comparison is scoped to the host OS recorded when the follower
    /// was admitted [ORB-16432]. A different OS is a different validation
    /// environment: the owner's base run is not started and cannot contradict
    /// the report. After the same owner-base history check, the owner accepts
    /// the follower's reported failures for that OS and keeps the handoff,
    /// including a pull request the leaf opened.
    pub(super) fn observe_known_red(
        &self,
        handoff: &TaskHandoff,
        observation: &HandoffObservation,
        deadline: std::time::Instant,
    ) -> Result<OwnerKnownRed, OrbitError> {
        let base_sha = &observation.candidate.base.commit;
        let reported = handoff
            .validation
            .iter()
            .filter_map(|reference| {
                let artifact = self
                    .get_task_artifact(&handoff.task_id, &reference.path)
                    .ok()
                    .flatten()
                    .filter(|artifact| sha256_hex(&artifact.content) == reference.sha256)?;
                let log: HandoffValidationLog = serde_json::from_slice(&artifact.content).ok()?;
                let report = log.known_red?;
                Some((log.command, log.exit_code, report))
            })
            .collect::<Vec<_>>();
        if reported.is_empty() {
            return Ok(OwnerKnownRed::default());
        }
        // The owner's own base, as admission captured it: never the branch a
        // handoff or its pull request names.
        let receipt = self.claim_receipt(&handoff.claim_id)?;
        let follower_os = receipt.request.os;
        let owner_os = self.host_os();
        // Both sides known and different: the owner's run is another
        // validation environment. An unrecorded OS stays comparable, which is
        // how a same-host admission that predates the field is still judged.
        let across_os = match (owner_os, follower_os) {
            (Some(owner), Some(follower)) if owner != follower => {
                Some(KnownRedOsAcceptance { follower, owner })
            }
            _ => None,
        };
        let ship = receipt.request.ship;
        let base_sync = if ship.mode == "pr" {
            BaseSyncMode::Remote
        } else {
            BaseSyncMode::Local
        };
        let mut known_red = OwnerKnownRed::default();
        for (command, exit_code, report) in reported {
            let verdict = if !observation.required_commands.contains(&command) {
                Err(KnownRedUnconfirmed::Inconclusive(
                    "it is not a command this owner requires, so the owner runs none".into(),
                ))
            } else if report.base_sha != *base_sha {
                Err(KnownRedUnconfirmed::Inconclusive(format!(
                    "the report names base {}",
                    report.base_sha
                )))
            } else if across_os.is_some() {
                orbit_engine::verify_claimed_known_red_base(
                    &self.paths().repo_root,
                    &ship.base_branch,
                    base_sync,
                    base_sha,
                )
                .map(|()| report.failures.clone())
            } else {
                orbit_engine::verify_claimed_known_red(
                    self,
                    &self.paths().repo_root,
                    &orbit_engine::ClaimedKnownRed {
                        base_branch: &ship.base_branch,
                        base_sync,
                        base_sha,
                        command: &command,
                        selection: report.selection.as_ref(),
                        exit_code,
                        failures: &report.failures,
                    },
                    deadline,
                )
            };
            let accepted_across_os = across_os.filter(|_| verdict.is_ok());
            if let Err(KnownRedUnconfirmed::Contradicted(reason)) = &verdict {
                known_red
                    .contradicted
                    .push(format!("required validation `{command}`: {reason}"));
            }
            known_red.observations.push(KnownRedObservation {
                command,
                base_sha: base_sha.clone(),
                verdict: verdict.map_err(|unconfirmed| unconfirmed.reason().to_string()),
                accepted_across_os,
            });
        }
        Ok(known_red)
    }
}
