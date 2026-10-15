//! Review contract and pinned certificate validation.
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::workflow::{
    BaselineRedFailures, EvidenceHostOs, KnownRedValidation, ReviewCertificate, ReviewEvidenceKind,
    ReviewExternalEvidence, ReviewTiming, ValidationOutcome, ValidationRole, handoff::*,
};

use super::super::lifecycle::invalid;
use super::{TaskCommitBoundary, object_id};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Judge a handoff's review disposition against the review contract its
    /// claim captured [ORB-13895]. Acceptance passes the owner's observation
    /// and requires it for before-PR evidence; approval and landing recheck
    /// the pinned evidence, and any observation they carry, the same way.
    pub(super) fn validate_handoff_review(
        &self,
        handoff: &TaskHandoff,
        ship: &AdmissionShipContract,
        observed: Option<&HandoffReviewObservation>,
        require_observation: bool,
    ) -> Result<(), OrbitError> {
        use HandoffReviewRefusal as R;
        // No candidate or PR exists to review. The owner independently
        // verifies the clean-base report and, unless it is a
        // `no-diff-expected` skip, required validation instead.
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
            if ship.before_pr
                || ship.before_landing
                || handoff.review != HandoffReview::not_required()
            {
                return Err(review_refused(
                    R::ReviewEvidenceUnexpected,
                    "the claim captured no review contract",
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
        // The evidence must be for the timing the claim captured: a
        // before-PR review cannot stand in for a before-landing one, nor the
        // reverse [ORB-14849].
        let captured = ship.review_timing();
        let evidence = match handoff.review.evidence() {
            Some(evidence)
                if captured != ReviewTiming::None
                    && handoff.review.policy == captured
                    && handoff.review.evidence_timing() == captured =>
            {
                evidence
            }
            _ => {
                return Err(review_refused(
                    R::ReviewEvidenceMissing,
                    &format!(
                        "the claim captured review timing `{}`; the handoff must carry the \
                         leaf's review for it",
                        captured.as_str()
                    ),
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
        // [ORB-15677] A check the certificate owes the owner instead of
        // holding is one the owner's captured rule declared a pull request's
        // hosted check runs, on a before-landing review whose landing waits
        // for that check. Anything else would land unverified.
        for owed in &certificate.owner_owed_evidence {
            let declared = captured == ReviewTiming::BeforeLanding
                && contract.host_evidence.iter().any(|rule| {
                    ((rule.kind == ReviewEvidenceKind::CodeQl && rule.os == EvidenceHostOs::Linux)
                        || rule.kind == ReviewEvidenceKind::NativeOs)
                        && owed.kind == rule.kind
                        && owed.os == Some(rule.os)
                        && rule.command == owed.command
                        && rule.artifact == owed.artifact
                        && rule.hosted_check.is_some()
                        && rule.hosted_check == owed.hosted_check
                });
            if !declared {
                return Err(certificate_refused(&format!(
                    "owner-owed `{}` is not a before-landing check the owner's captured \
                     host-evidence rules declare a hosted check runs",
                    owed.command
                )));
            }
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
                    && validation.executed_outcome() == ValidationOutcome::Passed
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
}

/// With the owner's observations (`observed`, at acceptance), the owner's own
/// base run must share exactly the reported failures; without them the
/// pinned report is only checked for shape.
pub(super) fn known_red_validation(
    log: &HandoffValidationLog,
    report: &HandoffKnownRed,
    handoff: &TaskHandoff,
    observed: Option<&[KnownRedObservation]>,
) -> Result<KnownRedValidation, OrbitError> {
    let refused = |detail: String| {
        invalid(&format!(
            "known_red_unverified: required validation '{}' failed on candidate {}; {detail}",
            log.command, log.tested_head
        ))
    };
    if report.base_sha != handoff.candidate.base.commit || report.failures.is_empty() {
        return Err(refused(format!(
            "its report must name the failures it shares with base {}",
            handoff.candidate.base.commit
        )));
    }
    if let Some(observed) = observed {
        let observation = observed
            .iter()
            .find(|observation| {
                observation.command == log.command && observation.base_sha == report.base_sha
            })
            .ok_or_else(|| {
                refused(format!(
                    "the owner has not run it on base {} itself",
                    report.base_sha
                ))
            })?;
        match &observation.verdict {
            Ok(failures) if *failures == report.failures => {}
            Ok(_) => {
                return Err(refused(format!(
                    "the owner's check of base {} judged other failures than the report names",
                    report.base_sha
                )));
            }
            Err(reason) => {
                return Err(refused(format!(
                    "the owner's run on base {} does not show the base fails them: {reason}",
                    report.base_sha
                )));
            }
        }
    }
    Ok(KnownRedValidation {
        command: log.command.clone(),
        base_sha: report.base_sha.clone(),
        tested_head: log.tested_head.clone(),
        run_id: log.run_id.clone(),
        failures: BaselineRedFailures::new(report.failures.iter().cloned(), ""),
    })
}

pub(super) fn review_refused(refusal: HandoffReviewRefusal, detail: &str) -> OrbitError {
    invalid(&format!("{}: {detail}", refusal.as_str()))
}
