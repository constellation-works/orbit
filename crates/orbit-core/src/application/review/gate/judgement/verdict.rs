//! Reconciling the verdict with external evidence and validation.

use std::collections::BTreeMap;

use orbit_automation::review::{
    ValidationContext, ValidationDefect, mutation_targets, validation_evidence,
};
use orbit_common::OrbitError;
use orbit_engine::review_gate::path_changed_between;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    CommitIdentity, FindingDisposition, ReviewExternalEvidence, ReviewVerdict, ValidationOutcome,
    ValidationRole,
};

use crate::OrbitRuntime;

use super::super::context::GateContext;

use super::Judgement;

impl Judgement {
    /// Resolve a repeated evidence-only report from durable result/log pairs
    /// on the final tree, never from the admission's advisory snapshot. Pairs
    /// on an earlier tree count through `carry` only: its patch is unchanged.
    /// `host` holds the results this settlement's own host just produced
    /// ([`Self::fulfil_host_evidence`]), which count like durable ones.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::application::review::gate) fn reconcile_external_evidence(
        &mut self,
        runtime: &OrbitRuntime,
        context: &GateContext,
        candidate: &SourceRevision,
        repair: Option<&CommitIdentity>,
        scope: &[String],
        carry: Option<&orbit_types::workflow::ReviewEvidenceCarried>,
        host: BTreeMap<String, ReviewExternalEvidence>,
    ) -> Result<(), OrbitError> {
        if context.task_ids.len() != 1
            || self.external_evidence.is_empty()
            || open_findings(&self.findings).next().is_some()
        {
            return Ok(());
        }
        let Some(requirements) =
            super::super::super::evidence::canonical_requirements(&self.external_evidence)
        else {
            return Ok(());
        };
        self.external_evidence = requirements;
        let Some(validation) = super::super::super::evidence::with_external_checks_passed(
            &self.validation,
            &self.external_evidence,
        ) else {
            return Ok(());
        };
        if validation_evidence(
            &validation,
            &ValidationContext {
                scope,
                obligations: &self.retained_obligations,
                retired: &self.retired_validation,
                required_validation_commands: self.required_validation_commands.as_deref(),
                baseline_commands: &self.baseline_commands,
            },
        )
        .is_err()
        {
            return Ok(());
        }
        let mut satisfied = super::super::super::evidence::satisfied_external_evidence(
            runtime,
            &context.task_ids[0],
            candidate,
        )?;
        satisfied.extend(host);
        let carried = super::super::super::evidence::carried_external_evidence(
            runtime,
            &context.task_ids[0],
            candidate,
            carry,
        )?;
        let mut used_carry = false;
        self.external_evidence.retain(|required| {
            let matching = |(_, evidence): &(&String, &ReviewExternalEvidence)| {
                evidence.matches_requirement(required, candidate)
            };
            let (found, via_carry) = match satisfied.iter().find(matching) {
                Some(found) => (found, None),
                None => match carried.iter().find(matching) {
                    Some(found) => (found, carry),
                    None => return true,
                },
            };
            let (artifact, evidence) = found;
            used_carry |= via_carry.is_some();
            for record in &mut self.validation {
                if record.command == required.command && record.role == ValidationRole::Required {
                    record.outcome = ValidationOutcome::Passed;
                    // The external run executed any path the reviewer's run deferred.
                    record.deferred.clear();
                    let carried = via_carry
                        .map(|carry| {
                            format!(
                                " (carried from tree {} by unchanged patch {})",
                                carry.from_tree, carry.patch_id
                            )
                        })
                        .unwrap_or_default();
                    let note = format!(
                        "External result {artifact}; log {}; tree {}{carried}",
                        evidence.log_artifact, candidate.tree,
                    );
                    record.note = Some(match record.note.take() {
                        Some(previous) => format!("{previous}; {note}"),
                        None => note,
                    });
                }
            }
            false
        });
        if used_carry {
            self.evidence_carried = carry.cloned();
        }
        if self.external_evidence.is_empty() {
            self.verdict = if repair.is_some() {
                ReviewVerdict::AcceptWithFixes
            } else {
                ReviewVerdict::Accept
            };
            self.escalation = None;
        }
        Ok(())
    }

    /// [ORB-14616] What keeps a file a control says it temporarily mutated
    /// from being restored, if anything: the final candidate must carry each
    /// target byte-identical to the `reviewed` candidate. [ORB-14632] The
    /// repository compares each target's blob between the two commits, with
    /// no rename detection, so a target left modified, deleted or moved away
    /// is caught however the review's commit records it; a target that is not
    /// a repository-relative path cannot be compared and is refused.
    pub(in crate::application::review::gate) fn unrestored_mutation(
        &self,
        workspace_path: &std::path::Path,
        reviewed: &str,
        final_candidate: &str,
    ) -> Result<Option<ValidationDefect>, OrbitError> {
        let targets = match mutation_targets(&self.validation) {
            Ok(targets) => targets,
            Err(defect) => return Ok(Some(defect)),
        };
        for (command, path) in targets {
            if path_changed_between(workspace_path, reviewed, final_candidate, &path)? {
                return Ok(Some(ValidationDefect::MutationTargetChanged {
                    command: command.to_string(),
                    target: path,
                }));
            }
        }
        Ok(None)
    }

    /// Cross-check the claimed verdict against what actually happened.
    /// `scope` is what validation sources are judged against: every task
    /// selector plus the candidate's changed paths. `unrestored` is what
    /// [`Self::unrestored_mutation`] found against the repository.
    pub(in crate::application::review::gate) fn reconcile_verdict(
        &mut self,
        repair: Option<&CommitIdentity>,
        scope: &[String],
        unrestored: Option<&ValidationDefect>,
    ) {
        if let Some(defect) = unrestored {
            self.downgrade(&defect.reason());
        }
        let open_findings = open_findings(&self.findings).count();
        match self.verdict {
            ReviewVerdict::Accept if repair.is_some() => self.downgrade(
                "verdict_inconsistent: the reviewer reported no fixes but changed the worktree",
            ),
            ReviewVerdict::AcceptWithFixes if repair.is_none() => self
                .downgrade("verdict_inconsistent: the reviewer reported fixes but changed nothing"),
            ReviewVerdict::Accept | ReviewVerdict::AcceptWithFixes if open_findings > 0 => {
                self.downgrade(&format!(
                    "verdict_inconsistent: {open_findings} finding(s) remain open under an accept"
                ));
            }
            ReviewVerdict::Reject if self.escalation.is_none() => {
                self.escalation = Some("reject".to_string());
            }
            _ => {}
        }
        // A pass rests on what the records establish, not on their count:
        // a required check must have passed, while a declared negative
        // control, an excluded action, a superseded attempt and a diagnostic
        // carry their own consistency rules, and no required check an
        // earlier report revision recorded may be dropped. Delivery coverage
        // reads the same function over the certificate's own scope and
        // retained obligations.
        if self.verdict.passed() {
            match self.validation_defect(scope) {
                None => self.validation_complete = true,
                Some(defect) => self.downgrade(&defect.reason()),
            }
        }
    }

    /// What keeps the records of a passing verdict from establishing the
    /// candidate over `scope`, if anything.
    fn validation_defect(&self, scope: &[String]) -> Option<ValidationDefect> {
        let context = ValidationContext {
            scope,
            obligations: &self.retained_obligations,
            retired: &self.retired_validation,
            required_validation_commands: self.required_validation_commands.as_deref(),
            baseline_commands: &self.baseline_commands,
        };
        validation_evidence(&self.validation, &context).err()
    }

    /// [ORB-14616] The defect a reviewer that just returned can still correct
    /// in its report: a passing verdict whose records fail only in their
    /// shape ([`ValidationDefect::correctable`]) over `scope`. A report that
    /// does not claim a pass, or fails on what its checks observed, has
    /// nothing to correct before settlement.
    pub(in crate::application::review::gate) fn correctable_defect(
        &self,
        scope: &[String],
    ) -> Option<ValidationDefect> {
        if !self.verdict.passed() {
            return None;
        }
        mutation_targets(&self.validation)
            .err()
            .or_else(|| self.validation_defect(scope))
            .filter(ValidationDefect::correctable)
    }

    /// [ORB-15130] Whether the reviewer left nothing but its initial
    /// provisional report: an `incomplete` verdict with no escalation,
    /// finding, validation record or evidence requirement, which the reviewer
    /// never updated. A genuine incomplete names what blocked it, records a
    /// check, or revises its report, and a host refusal is its own reason;
    /// none of those is abandoned work.
    pub(in crate::application::review::gate) fn abandoned_placeholder(&self) -> bool {
        self.initial_report_only
            && self.verdict == ReviewVerdict::Incomplete
            && !self.host_refused
            && self
                .escalation
                .as_deref()
                .is_none_or(|reason| reason.trim().is_empty())
            && self.findings.is_empty()
            && self.validation.is_empty()
            && self.external_evidence.is_empty()
            && self.retained_obligations.is_empty()
    }

    pub(in crate::application::review::gate) fn downgrade(&mut self, reason: &str) {
        self.host_refused = true;
        self.verdict = ReviewVerdict::Incomplete;
        self.external_evidence.clear();
        self.validation_complete = false;
        self.escalate(reason);
    }

    pub(in crate::application::review::gate) fn escalate(&mut self, reason: &str) {
        self.escalation = Some(match self.escalation.take() {
            Some(existing) if !existing.is_empty() => format!("{existing}; {reason}"),
            _ => reason.to_string(),
        });
    }
}

fn open_findings(
    findings: &[orbit_types::workflow::ReviewFinding],
) -> impl Iterator<Item = &orbit_types::workflow::ReviewFinding> {
    findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Open)
}
