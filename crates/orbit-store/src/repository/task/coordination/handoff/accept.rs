//! Accept a typed handoff into the claim journal.
use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, TaskDecision,
};
use orbit_types::workflow::{KNOWN_RED_VALIDATION_EVENT, KnownRedValidation, handoff::*};

use super::super::lifecycle::{encode, invalid, row};
use super::review::review_refused;
use super::{HANDOFF, TaskCommitBoundary, object_id};
use crate::contracts::*;

impl TaskCommitBoundary {
    pub(in super::super) fn accept_typed_handoff(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff: &TaskHandoff,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<Vec<String>, OrbitError> {
        let observation = self.observe_handoff(auth, &handoff.candidate)?;
        let bound = state
            .bound_run
            .as_ref()
            .ok_or_else(|| invalid("bound run required"))?;
        if auth.operator
            || state.claim.phase != ExecutionClaimPhase::Running
            || handoff.schema_version != 1
            || handoff.workspace_id != self.workspace_id
            || handoff.task_id != auth.task_id
            || handoff.claim_id != auth.claim_id
            || handoff.machine_id != bound.machine_id
            || handoff.run_id != bound.run_id
            || handoff.execution_summary.trim().is_empty()
            || handoff
                .execution_summary
                .lines()
                .find(|s| !s.trim().is_empty())
                .map(str::trim)
                == Some("Outcome: failed")
        {
            return Err(invalid("invalid typed handoff identity or summary"));
        }
        let candidate = &handoff.candidate;
        if [
            &candidate.repository,
            &candidate.source_branch,
            &candidate.base_branch,
            &candidate.landing_branch,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
            || [
                &candidate.candidate.commit,
                &candidate.candidate.tree,
                &candidate.base.commit,
                &candidate.base.tree,
            ]
            .iter()
            .any(|s| !object_id(s))
        {
            return Err(invalid(
                "exact repository, branches and object IDs required",
            ));
        }
        let ship = self.claim_ship(state)?;
        if let Some(review) = &ship.review
            && review.required_validation_commands.as_deref()
                != Some(observation.required_commands.as_slice())
        {
            return Err(review_refused(
                HandoffReviewRefusal::ReviewContractMismatch,
                "owner required validation changed after claim admission; dispatch a fresh claim under the current host contract",
            ));
        }
        if handoff.footprint_widening != observation.footprint_widening {
            return Err(invalid(&format!(
                "footprint widening differs from owner-observed diff: requested={:?}, observed={:?}",
                handoff.footprint_widening, observation.footprint_widening
            )));
        }
        // Agents may change any path the work requires; a footprint lock is
        // a scheduling hint, not a delivery gate. A concurrent task that
        // holds an added path meets it as a rebase conflict, which conflict
        // recovery resolves. Only paths no owner can track are refused.
        let mut footprint = state.claim.footprint.clone();
        if !handoff.footprint_widening.is_empty() {
            let checkout = self
                .registry
                .find_workspace_checkout(&self.workspace_id)?
                .ok_or_else(|| invalid("claimed workspace checkout unavailable"))?;
            let mut additions = BTreeSet::new();
            for path in &handoff.footprint_widening {
                if !orbit_common::fs::selector::claim_new_path_is_safe(path)
                    || !additions.insert(format!("file:{path}"))
                {
                    return Err(invalid(&format!(
                        "footprint widening refused path: {path}; paths must be unique and exclude \
                         traversal, Git or `.orbit` metadata and environment files (including `.envrc`); \
                         protected names and environment patterns ignore ASCII case on every host"
                    )));
                }
                // Missing owner worktree files are normal for published candidates;
                // existing symlink ancestors must not redirect selector identity.
                let canonical = orbit_common::fs::selector::canonical_selector_in_workspace(
                    &format!("file:{path}"),
                    &checkout.repo_root,
                )
                .map_err(|e| invalid(&format!("footprint widening refused path {path}: {e}")))?;
                if canonical != format!("file:{path}") {
                    return Err(invalid(&format!(
                        "footprint widening refused redirected path: {path}"
                    )));
                }
            }
            let additions = additions.into_iter().collect::<Vec<_>>();
            let bundle = self.bundle_store.read_bundle_lightweight(&auth.task_id)?;
            let mut context = bundle.envelope.context_files;
            for selector in &additions {
                if !context.contains(selector) {
                    context.push(selector.clone());
                }
                if !footprint.contains(selector) {
                    footprint.push(selector.clone());
                }
            }
            effects.worker_update = Some(ClaimWorkerUpdate {
                context_files: Some(context),
                ..Default::default()
            });
            // The handoff carries no per-path step, and a follower's commits
            // are host-made from its implementer's tree, so a claimed
            // attempt's additions are recorded as the implementer's.
            let widening = ContextFilesWidening {
                run_id: handoff.run_id.clone(),
                step: ContextWideningStep::Implement,
                activity: "claim_handoff".into(),
                selectors: additions,
            };
            params
                .append_history
                .push(orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: params.actor.clone(),
                    event: CONTEXT_FILES_WIDENED_EVENT.into(),
                    note: Some(encode(&widening)?),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::ContextFilesWidened(widening)),
                });
        }
        if candidate.base_branch != ship.base_branch
            || candidate.landing_branch != ship.landing_branch
            || !matches!(
                (&candidate.delivery, ship.mode.as_str()),
                (HandoffDelivery::PullRequest { number: 1.. }, "pr")
                    | (HandoffDelivery::LocalCandidate, "local")
                    | (HandoffDelivery::AlreadyLanded { .. }, "pr" | "local")
                    | (HandoffDelivery::NoDiff { .. }, "pr" | "local")
            )
        {
            return Err(invalid("handoff differs from captured ship contract"));
        }
        self.validate_handoff_review(handoff, &ship, observation.review.as_ref(), true)?;
        let known_red = self.validate_handoff_evidence(
            handoff,
            &observation.required_commands,
            Some(&observation.known_red),
        )?;
        // [ORB-15427] The owner established each known-red command against
        // its own base run, or accepted the follower's base evidence because
        // the follower was admitted on another OS [ORB-16432]. The task's
        // history names those tests, as the owner's own delivery records them.
        for known in &known_red {
            let across_os = observation
                .known_red
                .iter()
                .find(|item| item.command == known.command && item.base_sha == known.base_sha)
                .and_then(|item| item.accepted_across_os);
            params
                .append_history
                .push(orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: params.actor.clone(),
                    event: KNOWN_RED_VALIDATION_EVENT.into(),
                    note: Some(known.text(&known_red_acceptance_detail(known, across_os))),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::KnownRedValidation(known.clone())),
                });
        }
        for reference in &handoff.validation {
            let log: HandoffValidationLog =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                    .map_err(|error| invalid(&format!("invalid validation log: {error}")))?;
            if let Some(flaky) = log.flaky {
                params
                    .append_history
                    .push(orbit_types::task::TaskHistoryEntry {
                        at: Utc::now(),
                        by: params.actor.clone(),
                        event: orbit_types::workflow::FLAKY_VALIDATION_EVENT.into(),
                        note: Some(flaky.text()),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: Some(TaskDecision::FlakyValidation(flaky)),
                    });
            }
        }
        // A claimed worker cannot write its task's relations or read other
        // tasks, so the culprits it attributed the regression to are checked
        // here as an ordinary relation write would be, and recorded as
        // `regression_from` in this same commit.
        if !handoff.regression_from.is_empty() {
            let mut relations = self
                .bundle_store
                .read_bundle_lightweight(&auth.task_id)?
                .envelope
                .relations;
            for target in &handoff.regression_from {
                let relation = orbit_types::task::TaskRelation {
                    relation_type: orbit_types::task::TaskRelationType::RegressionFrom,
                    target: target.clone(),
                };
                if !relations.contains(&relation) {
                    relations.push(relation);
                }
            }
            self.registry
                .validate_task_relations(&self.workspace_id, &auth.task_id, &relations)
                .map_err(|error| {
                    invalid(&format!(
                        "the handoff's regression_from attribution was refused: {error}"
                    ))
                })?;
        }
        effects.regression_from = handoff.regression_from.clone();
        // The owner admitted these against its pilot's post-deploy finding
        // before this commit; the history entry is the task's owed record.
        if !handoff.owed_criteria.is_empty() {
            let owed = orbit_types::workflow::PostDeployOwed {
                run_id: handoff.run_id.clone(),
                criteria: handoff.owed_criteria.clone(),
            };
            params
                .append_history
                .push(orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: params.actor.clone(),
                    event: orbit_types::workflow::POST_DEPLOY_OWED_EVENT.into(),
                    note: Some(encode(&owed)?),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                });
        }
        let accepted = AcceptedHandoff {
            handoff_id: format!("handoff-{}", sha256_hex(encode(handoff)?.as_bytes())),
            handoff: handoff.clone(),
            required_commands: observation.required_commands.clone(),
            accepted_at: Utc::now(),
        };
        params.rows.push(row(HANDOFF, &auth.claim_id, &accepted)?);
        // A `done` contract names the owner policy it was admitted under. The
        // handoff is authorized only while the owner's own configuration,
        // observed for this decision, still grants that same policy. When the
        // owner has since withdrawn it, the valid delivery is still accepted
        // and waits in review for an operator, rather than discarding
        // validated work over a setting that changed after admission.
        if ship.completion == "done"
            && let Some(reference) = ship.authorization_reference.as_deref()
            && observation.owner_completion_authority.as_deref() == Some(reference)
        {
            self.add_handoff_authorization(
                &accepted,
                reference,
                HandoffAuthorizationSource::OwnerPolicy {
                    reference: reference.to_string(),
                },
                params,
            )?;
        }
        Ok(footprint)
    }
}

/// The history detail for one accepted known-red command. A same-OS owner
/// run names that run. A cross-OS acceptance names both operating systems
/// and keeps the leaf's pull request with the handoff [ORB-16432].
fn known_red_acceptance_detail(
    known: &KnownRedValidation,
    across_os: Option<KnownRedOsAcceptance>,
) -> String {
    let failures = known
        .failures
        .describe()
        .unwrap_or_else(|| "no failure named".to_string());
    match across_os {
        Some(scope) => format!(
            "Required validation '{}' fails on claimed candidate {} only with tests that fail on \
             the {} follower's base {}. This owner runs {}, a different validation environment, \
             so its own base run neither confirms nor contradicts that report. The follower's \
             base evidence was accepted and the handoff kept, so this delivery stays, including \
             a pull request the leaf opened; {failures}.",
            known.command, known.tested_head, scope.follower, known.base_sha, scope.owner
        ),
        None => format!(
            "Required validation '{}' fails on claimed candidate {} only with tests that also \
             fail when this owner runs it on base {}, so the candidate introduced none of them \
             and the handoff was accepted; {failures}.",
            known.command, known.tested_head, known.base_sha
        ),
    }
}
