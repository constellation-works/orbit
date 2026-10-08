//! Typed handoff acceptance and completion authority on the claim journal.
use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep};
use orbit_types::workflow::{
    ReviewCertificate, ReviewEvidenceKind, ReviewExternalEvidence, ReviewTiming, ValidationOutcome,
    ValidationRole, handoff::*,
};

use super::TaskCommitBoundary;
use super::lifecycle::{decode, encode, invalid, row};
use crate::contracts::*;

const HANDOFF: &str = "distributed-handoff-v1";
const AUTHORIZATION: &str = "distributed-handoff-authorization-v1";
const REVOCATION: &str = "distributed-handoff-revocation-v1";
const START: &str = "distributed-landing-start-v1";

impl TaskCommitBoundary {
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(START)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    /// Every handoff this owner accepted, one per claim. Read-only; delivery
    /// attribution matches a landed pull request against these to name the
    /// task it delivered.
    pub fn accepted_handoffs(&self) -> Result<Vec<AcceptedHandoff>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(HANDOFF)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    pub fn accepted_handoff(&self, claim_id: &str) -> Result<AcceptedHandoff, OrbitError> {
        self.find_accepted_handoff(claim_id)?
            .ok_or_else(|| invalid("typed handoff unavailable"))
    }

    /// The claim's accepted handoff, or `None` when none was accepted, so
    /// callers can tell an absent handoff from a failed read.
    pub fn find_accepted_handoff(
        &self,
        claim_id: &str,
    ) -> Result<Option<AcceptedHandoff>, OrbitError> {
        self.coordination_row(HANDOFF, claim_id)?
            .map(|r| decode(&r.payload_json))
            .transpose()
    }

    /// The certificate an accepted before-PR handoff carries, read back
    /// through the digest the handoff pinned, or `None` for a handoff that
    /// carries no before-PR review [ORB-13895].
    pub fn accepted_review_certificate(
        &self,
        claim_id: &str,
    ) -> Result<Option<ReviewCertificate>, OrbitError> {
        self.enter_ordinary(|| {
            let Some(accepted) = self.find_accepted_handoff(claim_id)? else {
                return Ok(None);
            };
            let Some(evidence) = accepted.handoff.review.before_pr() else {
                return Ok(None);
            };
            let bytes = self.artifact_bytes(&accepted.handoff.task_id, &evidence.certificate)?;
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| invalid(&format!("invalid review certificate: {e}")))
        })
    }

    /// The ship contract the claim was admitted under.
    fn claim_ship(&self, state: &ClaimInspection) -> Result<AdmissionShipContract, OrbitError> {
        let AdmissionLookup::Found { receipt, .. } = self.lookup_admission(
            &AdmissionIdentity::trusted_local(state.claim.executed_on.clone()),
            &state.claim.request_id,
        )?
        else {
            return Err(invalid("claim receipt unavailable"));
        };
        receipt
            .claim
            .as_ref()
            .ok_or_else(|| invalid("claim receipt unavailable"))?;
        Ok(receipt.request.ship)
    }

    /// Judge a handoff's review disposition against the review contract its
    /// claim captured [ORB-13895]. Acceptance passes the owner's observation
    /// and requires it for before-PR evidence; approval and landing recheck
    /// the pinned evidence, and any observation they carry, the same way.
    fn validate_handoff_review(
        &self,
        handoff: &TaskHandoff,
        ship: &AdmissionShipContract,
        observed: Option<&HandoffReviewObservation>,
        require_observation: bool,
    ) -> Result<(), OrbitError> {
        use HandoffReviewRefusal as R;
        // No candidate or PR exists to review. The owner independently
        // verifies the clean-base report and required validation instead.
        if matches!(handoff.candidate.delivery, HandoffDelivery::NoDiff { .. }) {
            if handoff.review != HandoffReview::not_required() {
                return Err(review_refused(
                    R::ReviewEvidenceUnexpected,
                    "NoDiff carries no before-PR review",
                ));
            }
            return Ok(());
        }
        let Some(contract) = &ship.review else {
            if ship.before_pr || handoff.review != HandoffReview::not_required() {
                return Err(review_refused(
                    R::ReviewEvidenceUnexpected,
                    "the claim captured no before-PR review contract",
                ));
            }
            return Ok(());
        };
        if contract.required_validation_commands.is_none() {
            return Err(review_refused(
                R::ReviewContractMismatch,
                "the captured review contract predates its required-check list; dispatch a fresh claim under the current host contract",
            ));
        }
        let evidence = match (handoff.review.policy, handoff.review.before_pr()) {
            (ReviewTiming::BeforePr, Some(evidence)) => evidence,
            _ => {
                return Err(review_refused(
                    R::ReviewEvidenceMissing,
                    "the claim captured review.before_pr; the handoff must carry the leaf's review",
                ));
            }
        };
        let head = &handoff.candidate.candidate;
        if !evidence.verdict.passed() {
            return Err(review_refused(
                R::ReviewNotPassed,
                &format!("verdict `{}` does not open a PR", evidence.verdict.as_str()),
            ));
        }
        if evidence.reviewed_head_sha != head.commit
            || evidence
                .reviewer_commit
                .as_ref()
                .is_some_and(|commit| *commit != head.commit)
        {
            return Err(review_refused(
                R::ReviewedHeadMismatch,
                &format!(
                    "reviewed head {} (reviewer commit {:?}) is not the handed-off candidate {}",
                    evidence.reviewed_head_sha, evidence.reviewer_commit, head.commit
                ),
            ));
        }
        if [
            &evidence.attempt_id,
            &evidence.reviewer_crew,
            &evidence.reviewer_run_id,
        ]
        .iter()
        .any(|s| s.trim().is_empty())
            || !object_id(&evidence.reviewed_base_sha)
        {
            return Err(review_refused(
                R::ReviewCertificateMismatch,
                "attempt, reviewer, run and an exact reviewed base are required",
            ));
        }
        if contract
            .crew
            .as_ref()
            .is_some_and(|crew| *crew != evidence.reviewer_crew)
        {
            return Err(review_refused(
                R::ReviewContractMismatch,
                &format!(
                    "reviewer crew `{}` is not the captured review crew `{}`",
                    evidence.reviewer_crew,
                    contract.crew.as_deref().unwrap_or_default()
                ),
            ));
        }
        match observed {
            Some(observed) if observed.reviewed_base_sha != evidence.reviewed_base_sha => {
                return Err(invalid("owner review observation names another base"));
            }
            Some(observed) if !observed.reviewed_base_is_ancestor => {
                return Err(review_refused(
                    R::ReviewedBaseNotAncestor,
                    &format!(
                        "reviewed base {} is not the candidate base {} or its ancestor",
                        evidence.reviewed_base_sha, handoff.candidate.base.commit
                    ),
                ));
            }
            Some(_) => {}
            None if require_observation => {
                return Err(invalid("trusted owner review observation required"));
            }
            None => {}
        }
        let certificate_refused =
            |detail: &str| review_refused(R::ReviewCertificateMismatch, detail);
        let bytes = |reference: &HandoffArtifactRef| {
            self.artifact_bytes(&handoff.task_id, reference)
                .map_err(|e| certificate_refused(&format!("{}: {e}", reference.path)))
        };
        for reference in &evidence.artifacts {
            bytes(reference)?;
        }
        let certificate: ReviewCertificate = serde_json::from_slice(&bytes(&evidence.certificate)?)
            .map_err(|e| certificate_refused(&format!("unreadable certificate: {e}")))?;
        if certificate.schema_version != contract.contract_version {
            return Err(review_refused(
                R::ReviewContractMismatch,
                &format!(
                    "certificate contract version {} is not the captured version {}",
                    certificate.schema_version, contract.contract_version
                ),
            ));
        }
        if certificate.required_validation_commands != contract.required_validation_commands {
            return Err(review_refused(
                R::ReviewContractMismatch,
                "certificate required checks differ from the owner's captured review contract",
            ));
        }
        if certificate.baseline_commands != contract.baseline_commands {
            return Err(review_refused(
                R::ReviewContractMismatch,
                "certificate baseline commands differ from the owner's captured review contract",
            ));
        }
        let reviewer_commit_matches = match &evidence.reviewer_commit {
            Some(commit) => certificate
                .repair_commits
                .last()
                .is_some_and(|repair| repair.commit == *commit),
            None => certificate.repair_commits.is_empty(),
        };
        if certificate.attempt_id != evidence.attempt_id
            || certificate.verdict != evidence.verdict
            || certificate.final_candidate != *head
            || certificate.base.commit != evidence.reviewed_base_sha
            || certificate.reviewer.crew != evidence.reviewer_crew
            || !certificate.task_ids.contains(&handoff.task_id)
            || !reviewer_commit_matches
            || observed.is_some_and(|observed| certificate.repository != observed.repository)
        {
            return Err(certificate_refused(
                "certificate does not bind this attempt, verdict, reviewer, task, repository, \
                 base and final candidate",
            ));
        }
        // [ORB-14478] Every host run the certificate counted is pinned, and
        // each pinned result binds this candidate's tree, its OS and a
        // passed required check of the certificate, with its pinned log.
        let pinned: BTreeMap<&str, &HandoffArtifactRef> = evidence
            .host_evidence
            .iter()
            .map(|reference| (reference.path.as_str(), reference))
            .collect();
        let mut counted = BTreeSet::new();
        for record in certificate
            .host_evidence
            .iter()
            .filter(|record| record.passed)
        {
            let host_refused = |detail: &str| {
                certificate_refused(&format!(
                    "host run `{}` ({}): {detail}",
                    record.command,
                    record.os.as_str()
                ))
            };
            let (Some(artifact), Some(log)) = (&record.artifact, &record.log_artifact) else {
                return Err(host_refused("names no result or log"));
            };
            let (Some(result_ref), Some(log_ref)) =
                (pinned.get(artifact.as_str()), pinned.get(log.as_str()))
            else {
                return Err(host_refused("the handoff does not pin its result and log"));
            };
            counted.extend([artifact.as_str(), log.as_str()]);
            let result: ReviewExternalEvidence = serde_json::from_slice(&bytes(result_ref)?)
                .map_err(|e| host_refused(&format!("unreadable result: {e}")))?;
            if bytes(log_ref)?.is_empty() {
                return Err(host_refused("empty log"));
            }
            let required_passed = certificate.validation.iter().any(|validation| {
                validation.command == record.command
                    && validation.role == ValidationRole::Required
                    && validation.outcome == ValidationOutcome::Passed
            });
            if result.schema_version != 1
                || result.kind != ReviewEvidenceKind::HostSandboxTest
                || result.outcome != ValidationOutcome::Passed
                || result.command != record.command
                || result.os != Some(record.os)
                || result.log_artifact != *log
                || result.candidate.tree != head.tree
                || record.tree != head.tree
                || !required_passed
            {
                return Err(host_refused(
                    "the result does not bind this command, OS, log, candidate tree and a \
                     passed required check",
                ));
            }
        }
        if pinned.len() != counted.len() {
            return Err(certificate_refused(
                "the handoff pins host evidence the certificate did not count",
            ));
        }
        Ok(())
    }

    fn artifact_bytes(
        &self,
        task_id: &str,
        reference: &HandoffArtifactRef,
    ) -> Result<Vec<u8>, OrbitError> {
        orbit_types::task::validate_relative_artifact_path(&reference.path)?;
        // Full bundle read verifies the manifest and payload digests before exposure.
        let bundle = self.bundle_store.read_bundle(task_id)?;
        let file = bundle
            .artifact_manifest
            .as_ref()
            .and_then(|m| m.files.iter().find(|f| f.path == reference.path))
            .ok_or_else(|| invalid("owner validation artifact missing"))?;
        if file.sha256 != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        let bytes = std::fs::read(
            self.bundle_store
                .bundle_path(task_id)?
                .join(orbit_types::task::TASK_ARTIFACTS_DIR_NAME)
                .join(&file.blob),
        )?;
        if sha256_hex(&bytes) != reference.sha256 {
            return Err(invalid("validation artifact changed"));
        }
        Ok(bytes)
    }

    fn validate_handoff_evidence(
        &self,
        handoff: &TaskHandoff,
        required: &[String],
    ) -> Result<(), OrbitError> {
        // An empty requirement list is no required check; any log the
        // handoff still carries must pin this claim's candidate all the same.
        let commands: BTreeSet<_> = required.iter().map(String::as_str).collect();
        if commands.iter().any(|c| c.trim().is_empty()) || commands.len() != required.len() {
            return Err(invalid("owner validation requirements blank or duplicated"));
        }
        let mut passed = BTreeSet::new();
        for reference in &handoff.validation {
            let log: HandoffValidationLog =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                    .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
            if log.schema_version != 1
                || log.workspace_id != handoff.workspace_id
                || log.task_id != handoff.task_id
                || log.claim_id != handoff.claim_id
                || log.machine_id != handoff.machine_id
                || log.run_id != handoff.run_id
                || log.tested_head != handoff.candidate.candidate.commit
                || log.candidate != handoff.candidate
                || log.exit_code != 0
                || log.command.trim().is_empty()
                || !passed.insert(log.command)
            {
                return Err(invalid(
                    "validation must pass for the exact claim/run/candidate/base",
                ));
            }
        }
        if !commands.iter().all(|c| passed.contains(*c)) {
            return Err(invalid("required validation missing"));
        }
        if let HandoffDelivery::NoDiff { evidence } = &handoff.candidate.delivery {
            let report: serde_json::Value =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, evidence)?)
                    .map_err(|e| invalid(&format!("invalid NoDiff verifier report: {e}")))?;
            if handoff.candidate.candidate != handoff.candidate.base
                || !handoff.footprint_widening.is_empty()
                || report["task_id"] != handoff.task_id
                || report["job_run_id"] != handoff.run_id
                || report["base_sha"] != handoff.candidate.base.commit
                || !matches!(
                    report["decision"].as_str(),
                    Some("verified_no_diff" | "verified_already_landed")
                )
            {
                return Err(invalid("NoDiff verifier report identity or base mismatch"));
            }
        }
        if let HandoffDelivery::AlreadyLanded {
            covering_commit,
            evidence,
        } = &handoff.candidate.delivery
        {
            let proof: AlreadyLandedEvidence =
                serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, evidence)?)
                    .map_err(|e| invalid(&format!("invalid already-landed evidence: {e}")))?;
            let bundle = self
                .bundle_store
                .read_bundle_lightweight(&handoff.task_id)?;
            let comments = bundle
                .comments
                .iter()
                .map(|c| orbit_types::task::TaskComment {
                    at: c.at,
                    by: c.by.clone(),
                    message: c.body.clone(),
                })
                .collect::<Vec<_>>();
            let task = crate::repository::task::TaskV2Store::new(
                self.registry.clone(),
                self.workspace_id.clone(),
            )
            .task_from_bundle(bundle)?;
            if proof.schema_version != 1
                || proof.task_id != handoff.task_id
                || proof.covering_task_id != handoff.task_id
                || proof.run_id != handoff.run_id
                || proof.tested_head != handoff.candidate.candidate.commit
                || proof.covering_commit != *covering_commit
                || !object_id(covering_commit)
                || handoff.candidate.candidate != handoff.candidate.base
                || proof.scope != already_landed_scope(&task, &comments)
                || proof.required_commands != required
                || proof.validation.len() != required.len()
                || task.acceptance_criteria.is_empty()
                || proof.criteria_evidence.len() != task.acceptance_criteria.len()
                || proof.criteria_evidence.iter().any(|s| s.trim().is_empty())
            {
                return Err(invalid(
                    "already-landed evidence identity, scope or criteria mismatch",
                ));
            }
            let mut checked = BTreeSet::new();
            for check in &proof.validation {
                if check.validation.outcome != orbit_types::workflow::ValidationOutcome::Passed
                    || check.validation.role != orbit_types::workflow::ValidationRole::Required
                    || !commands.contains(check.validation.command.as_str())
                    || !checked.insert(&check.validation.command)
                {
                    return Err(invalid("already-landed validation incomplete"));
                }
                let reference = handoff
                    .validation
                    .iter()
                    .find(|r| r.path == check.log_artifact)
                    .ok_or_else(|| invalid("already-landed validation log missing"))?;
                let log: HandoffValidationLog =
                    serde_json::from_slice(&self.artifact_bytes(&handoff.task_id, reference)?)
                        .map_err(|e| invalid(&format!("invalid validation log: {e}")))?;
                if log.command != check.validation.command {
                    return Err(invalid("already-landed validation log mismatch"));
                }
            }
        }
        Ok(())
    }

    fn observe_handoff<'a>(
        &self,
        auth: &'a ClaimInvocation,
        candidate: &HandoffCandidate,
    ) -> Result<&'a HandoffObservation, OrbitError> {
        let observation = auth
            .handoff_observation
            .as_ref()
            .ok_or_else(|| invalid("trusted owner candidate observation required"))?;
        if observation.candidate != *candidate {
            return Err(invalid("candidate or delivery identity changed"));
        }
        Ok(observation)
    }

    pub(super) fn accept_typed_handoff(
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
            params
                .append_history
                .push(orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: params.actor.clone(),
                    event: CONTEXT_FILES_WIDENED_EVENT.into(),
                    note: Some(encode(&ContextFilesWidening {
                        run_id: handoff.run_id.clone(),
                        step: ContextWideningStep::Implement,
                        activity: "claim_handoff".into(),
                        selectors: additions,
                    })?),
                    from_status: None,
                    to_status: None,
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
        self.validate_handoff_evidence(handoff, &observation.required_commands)?;
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

    fn add_handoff_authorization(
        &self,
        accepted: &AcceptedHandoff,
        approver: &str,
        source: HandoffAuthorizationSource,
        params: &mut TaskCoordinationCommitParams,
    ) -> Result<(), OrbitError> {
        let id = &accepted.handoff_id;
        if self
            .coordination_rows(AUTHORIZATION)?
            .iter()
            .any(|r| &r.row_id == id)
        {
            return Ok(()); // One immutable authorization and one start per exact handoff.
        }
        let authorization = HandoffAuthorization {
            authorization_id: format!("authorization-{id}"),
            handoff_id: id.clone(),
            workspace_id: self.workspace_id.clone(),
            task_id: accepted.handoff.task_id.clone(),
            claim_id: accepted.handoff.claim_id.clone(),
            candidate: accepted.handoff.candidate.clone(),
            approver: approver.into(),
            created_at: Utc::now(),
            source,
        };
        let start = LandingStartRequest {
            handoff_id: id.clone(),
            authorization_id: authorization.authorization_id.clone(),
            state: LandingStartState::Pending,
            created_at: authorization.created_at,
        };
        params.rows.push(row(AUTHORIZATION, id, &authorization)?);
        params.rows.push(row(START, id, &start)?);
        Ok(())
    }

    pub(super) fn approve_typed_handoff(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
        handoff_id: &str,
        candidate: &HandoffCandidate,
        params: &mut TaskCoordinationCommitParams,
    ) -> Result<(), OrbitError> {
        if !auth.operator
            || state.claim.phase != ExecutionClaimPhase::HandedOff
            || state.landing_invalidated
        {
            return Err(invalid("current operator approval capability required"));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        if accepted.handoff_id != handoff_id || accepted.handoff.candidate != *candidate {
            return Err(invalid("handoff candidate mismatch"));
        }
        let observed = self.observe_handoff(auth, candidate)?;
        if observed.required_commands != accepted.required_commands {
            return Err(invalid("validation requirements changed"));
        }
        self.validate_handoff_review(
            &accepted.handoff,
            &self.claim_ship(state)?,
            observed.review.as_ref(),
            false,
        )?;
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands)?;
        self.add_handoff_authorization(
            &accepted,
            &auth.machine_id,
            HandoffAuthorizationSource::Operator,
            params,
        )
    }

    pub(super) fn revoke_handoff_authority(
        &self,
        auth: &ClaimInvocation,
        reason: &str,
        params: &mut TaskCoordinationCommitParams,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        if !self
            .coordination_rows(HANDOFF)?
            .iter()
            .any(|r| r.row_id == auth.claim_id)
        {
            return Ok(());
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let id = &accepted.handoff_id;
        let Some(old) = self.coordination_row(START, id)? else {
            return Ok(());
        };
        let mut start: LandingStartRequest = decode(&old.payload_json)?;
        match start.state {
            LandingStartState::Revoked => return Ok(()),
            LandingStartState::Completed => {
                return Err(invalid("handoff has already landed"));
            }
            LandingStartState::Pending => {}
        }
        let revocation = HandoffRevocation {
            authorization_id: start.authorization_id.clone(),
            actor: auth.machine_id.clone(),
            reason: reason.into(),
            revoked_at: Utc::now(),
        };
        params.rows.push(row(REVOCATION, id, &revocation)?);
        start.state = LandingStartState::Revoked;
        effects.replacements.push((old, row(START, id, &start)?));
        Ok(())
    }

    /// The unrevoked completion authorization scoped to exactly this handoff
    /// and candidate. Every landing decision re-reads it; a historical row is
    /// never permission on its own.
    pub(super) fn current_landing_authorization(
        &self,
        accepted: &AcceptedHandoff,
    ) -> Result<HandoffAuthorization, OrbitError> {
        let raw = self
            .coordination_row(AUTHORIZATION, &accepted.handoff_id)?
            .ok_or_else(|| invalid("handoff awaits completion approval"))?;
        let authorization: HandoffAuthorization = decode(&raw.payload_json)?;
        if authorization.handoff_id != accepted.handoff_id
            || authorization.candidate != accepted.handoff.candidate
        {
            return Err(invalid("completion authorization scope mismatch"));
        }
        if self
            .coordination_rows(REVOCATION)?
            .iter()
            .any(|r| r.row_id == accepted.handoff_id)
        {
            return Err(invalid("landing authority revoked"));
        }
        Ok(authorization)
    }

    /// Move a pending outbox request to its settled state. Only a pending
    /// request settles: completion cannot resurrect a revoked request, and
    /// revocation cannot reopen a completed one.
    pub(super) fn settle_landing_request(
        &self,
        handoff_id: &str,
        state: LandingStartState,
        effects: &mut ClaimCommitEffects,
    ) -> Result<(), OrbitError> {
        let Some(old) = self.coordination_row(START, handoff_id)? else {
            return Err(invalid("no pending landing request for this handoff"));
        };
        let mut start: LandingStartRequest = decode(&old.payload_json)?;
        if start.state != LandingStartState::Pending {
            return Err(invalid("landing request is no longer pending"));
        }
        start.state = state;
        effects
            .replacements
            .push((old, row(START, handoff_id, &start)?));
        Ok(())
    }

    /// Must be rechecked when writing merge intent. Its return value alone never
    /// authorizes a future external action; the dependent consumer owns that protocol.
    pub(super) fn check_handoff_landing(
        &self,
        auth: &ClaimInvocation,
        state: &ClaimInspection,
    ) -> Result<(), OrbitError> {
        if !auth.operator || state.landing_invalidated {
            return Err(invalid("landing authority revoked"));
        }
        let accepted = self.accepted_handoff(&auth.claim_id)?;
        let observation = self.observe_handoff(auth, &accepted.handoff.candidate)?;
        if observation.required_commands != accepted.required_commands {
            return Err(invalid("validation requirements changed"));
        }
        // The landing merges exactly the accepted candidate, which acceptance
        // proved is the reviewed head; recheck the pinned review evidence too.
        self.validate_handoff_review(
            &accepted.handoff,
            &self.claim_ship(state)?,
            observation.review.as_ref(),
            false,
        )?;
        self.validate_handoff_evidence(&accepted.handoff, &accepted.required_commands)?;
        let authorization = self.current_landing_authorization(&accepted)?;
        if authorization.workspace_id != self.workspace_id
            || authorization.task_id != auth.task_id
            || authorization.claim_id != auth.claim_id
        {
            return Err(invalid("completion authorization scope mismatch"));
        }
        match &authorization.source {
            HandoffAuthorizationSource::Operator => {}
            // A policy authorization lands only while the owner still grants
            // that policy; withdrawing the key stops every unlanded handoff.
            HandoffAuthorizationSource::OwnerPolicy { reference } => {
                if observation.owner_completion_authority.as_deref() != Some(reference.as_str()) {
                    return Err(invalid(
                        "owner completion policy withdrawn; restore `workflow.distributed_completion = \"done\"` \
                         or revoke this handoff",
                    ));
                }
            }
            // A grant-sourced authorization persisted before operation mode
            // was removed can no longer be rechecked, so it fails closed.
            HandoffAuthorizationSource::Grant { .. } => {
                return Err(invalid("managed completion unsupported"));
            }
        }
        Ok(())
    }
}

fn review_refused(refusal: HandoffReviewRefusal, detail: &str) -> OrbitError {
    invalid(&format!("{}: {detail}", refusal.as_str()))
}

fn object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}
