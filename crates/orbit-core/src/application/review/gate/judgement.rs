//! Check the reviewer's claims against the repository and the task scope,
//! and render the findings comment and the PR's review-fixes section.

use std::collections::BTreeMap;

use orbit_automation::review::{
    ValidationContext, combined_task_meaning_digest, task_meaning_digest, validation_evidence,
    validation_limitations, validation_role_counts,
};
use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_common::security::release::sha256_hex;
use orbit_engine::review_gate::{
    REVIEW_ATTEMPT_TRAILER, commit_reviewer_repairs, uncommitted_paths,
};
use orbit_types::task::{ContextWideningStep, Task, TaskArtifact};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    CommitIdentity, FindingDisposition, REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, RetainedObligation, RetiredValidation, ReviewAttempt,
    ReviewCertificate, ReviewExternalEvidence, ReviewReport, ReviewReportHistory,
    ReviewReportRevision, ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome,
    ValidationRole,
};

use super::super::automation_error;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::context::GateContext;

/// The reviewer's claims, checked against the repository and the task scope.
pub(super) struct Judgement {
    pub(super) verdict: ReviewVerdict,
    pub(super) external_evidence: Vec<orbit_types::workflow::ReviewEvidenceRequirement>,
    pub(super) findings: Vec<orbit_types::workflow::ReviewFinding>,
    pub(super) validation: Vec<orbit_types::workflow::ReviewValidation>,
    pub(super) validation_complete: bool,
    pub(super) required_validation_commands: Option<Vec<String>>,
    /// Required-check records earlier report revisions of this attempt made
    /// that the final report does not repeat verbatim.
    pub(super) retained_obligations: Vec<RetainedObligation>,
    /// Retained record ids the final report retired, with their reasons.
    pub(super) retired_validation: Vec<RetiredValidation>,
    pub(super) escalation: Option<String>,
    summary: String,
    pub(super) task_meaning_digest: String,
    pub(super) selectors_widened: Vec<String>,
    /// [ORB-14450] Set when a requirement was satisfied by evidence on an
    /// earlier tree whose patch the final candidate carries unchanged.
    pub(super) evidence_carried: Option<orbit_types::workflow::ReviewEvidenceCarried>,
    /// [ORB-14434] Set once the host downgraded the review: a review the
    /// host found incomplete is never held for a red base.
    pub(super) host_refused: bool,
    /// [ORB-14478] `host_sandbox_test` requirements this host ran or refused.
    pub(super) host_evidence: Vec<orbit_types::workflow::HostEvidenceRecord>,
}

impl Judgement {
    /// Read the reports the reviewer persisted for this attempt on any of
    /// the bundle's tasks and merge them. Reports from before the attempt
    /// started are ignored; none at all, or one that is unreadable, names
    /// another contract, or names another attempt, is an incomplete review,
    /// never a pass. Benign shape drift is accepted ([`ReviewReport::parse`]).
    pub(super) fn from_report(
        runtime: &OrbitRuntime,
        context: &GateContext,
        attempt: &ReviewAttempt,
    ) -> Result<Self, OrbitError> {
        let task_meaning_digest = context.task_digests.1.clone();
        let incomplete = |reason: &str| Self {
            external_evidence: Vec::new(),
            verdict: ReviewVerdict::Incomplete,
            findings: Vec::new(),
            validation: Vec::new(),
            validation_complete: false,
            required_validation_commands: context
                .admission
                .as_ref()
                .and_then(|admission| admission.required_validation_commands.clone()),
            retained_obligations: Vec::new(),
            retired_validation: Vec::new(),
            escalation: Some(reason.to_string()),
            summary: String::new(),
            task_meaning_digest: task_meaning_digest.clone(),
            selectors_widened: Vec::new(),
            evidence_carried: None,
            host_refused: true,
            host_evidence: Vec::new(),
        };
        let mut reports = Vec::new();
        let mut revisions = Vec::new();
        let mut stale = false;
        for task_id in &context.task_ids {
            let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_REPORT_ARTIFACT)? else {
                continue;
            };
            let history = runtime.get_task_artifact(task_id, REVIEW_REPORT_HISTORY_ARTIFACT)?;
            match retained_revisions(task_id, history, attempt, &artifact.content) {
                Ok(kept) => revisions.extend(kept),
                Err(reason) => return Ok(incomplete(&reason)),
            }
            let manifest = runtime.get_task_artifact_manifest(task_id)?;
            if manifest
                .iter()
                .find(|file| file.path == REVIEW_REPORT_ARTIFACT)
                .is_some_and(|file| file.created_at < attempt.started_at)
            {
                stale = true;
                continue;
            }
            let report = match ReviewReport::parse(&artifact.content) {
                Ok(report) => report,
                Err(error) => {
                    return Ok(incomplete(&format!(
                        "report_unreadable: the report on {task_id}: {error}"
                    )));
                }
            };
            if report.schema_version != REVIEW_CONTRACT_VERSION {
                return Ok(incomplete(&format!(
                    "report_contract_mismatch: the report on {task_id} has schema_version {} \
                     instead of {REVIEW_CONTRACT_VERSION}",
                    report.schema_version
                )));
            }
            if report.attempt_id != attempt.attempt_id {
                return Ok(incomplete(&format!(
                    "report_attempt_mismatch: the report on {task_id} names attempt {} but {} \
                     was admitted",
                    report.attempt_id, attempt.attempt_id
                )));
            }
            reports.push(report);
        }
        let Some(report) = merge_reports(reports) else {
            return Ok(incomplete(if stale {
                "report_stale: review-report.json predates this attempt"
            } else {
                "report_missing: the reviewer persisted no review-report.json"
            }));
        };
        let retained_obligations = retained_obligations(revisions, &report.validation);
        Ok(Self {
            external_evidence: report.external_evidence,
            verdict: report.verdict,
            findings: report.findings,
            validation: report.validation,
            validation_complete: false,
            required_validation_commands: context
                .admission
                .as_ref()
                .and_then(|admission| admission.required_validation_commands.clone()),
            retained_obligations,
            retired_validation: report.retired_validation,
            escalation: report.escalation,
            summary: report.summary,
            task_meaning_digest,
            selectors_widened: Vec::new(),
            evidence_carried: None,
            host_refused: false,
            host_evidence: Vec::new(),
        })
    }

    /// Task criteria, scope, or contract changes during the review
    /// invalidate it. Selectors may grow — the reviewer through the task API,
    /// or Orbit widening for a path it changed; anything else re-establishes
    /// review.
    pub(super) fn check_task_meaning(
        &mut self,
        context: &GateContext,
        attempt: &ReviewAttempt,
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        if self.task_meaning_digest == attempt.task_meaning_digest {
            return Ok(());
        }
        if !selectors_only_grew(context, attempt, admitted_selectors)? {
            self.downgrade(
                "task_meaning_changed: task criteria, plan, scope, or relations changed \
                 during the review",
            );
        }
        Ok(())
    }

    /// Commit whatever the reviewer changed as its own attributed work: the
    /// candidate's one reviewer commit, `review: <summary>`, on top of the
    /// untouched implementation commits [ORB-13989].
    ///
    /// A reviewer may change any path a fix requires: a changed path no
    /// task selector covers widens the reviewed task's `context_files`, with
    /// review provenance in its history, and does not abandon the review.
    pub(super) fn commit_repairs(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        reviewer: &ReviewerIdentity,
        attempt: &ReviewAttempt,
    ) -> Result<Option<CommitIdentity>, OrbitError> {
        let changed = uncommitted_paths(&context.workspace_path)?;
        if changed.is_empty() {
            return Ok(None);
        }
        let out_of_scope = out_of_scope_paths(&changed, &context.tasks);
        if !out_of_scope.is_empty() {
            self.widen_reviewer_selectors(runtime, context, &out_of_scope)?;
        }
        let finding_ids = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Repaired)
            .map(|finding| finding.id.clone())
            .collect::<Vec<_>>();
        let message = format!(
            "review: {} [{}]\n\nFindings: {}\nPaths: {}\n{REVIEW_ATTEMPT_TRAILER}: {}\nOrbit-Review-Crew: {}",
            if self.summary.trim().is_empty() {
                "reviewer repairs".to_string()
            } else {
                self.summary
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string()
            },
            context.task_ids.join(", "),
            if finding_ids.is_empty() {
                "none named".to_string()
            } else {
                finding_ids.join(", ")
            },
            changed.join(", "),
            attempt.attempt_id,
            reviewer.crew,
        );
        // The provider names the agent family the repair commit is attributed
        // to; the model alone may carry no family hint.
        let commit = commit_reviewer_repairs(
            &context.workspace_path,
            &repair_author_label(reviewer),
            &message,
        )?;
        Ok(commit)
    }

    /// Adopt the repair commit an interrupted settlement of this attempt
    /// already made, widening its paths exactly as [`Self::commit_repairs`]
    /// did before committing. The interrupted run widened selectors before
    /// committing, so a repair path outside the admitted selectors is
    /// reported as widened even though it is in scope by now.
    pub(super) fn adopt_repairs(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        committed: &[String],
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        let out_of_scope = out_of_scope_paths(committed, &context.tasks);
        if !out_of_scope.is_empty() {
            self.widen_reviewer_selectors(runtime, context, &out_of_scope)?;
        }
        let admitted_tasks = context
            .tasks
            .iter()
            .map(|task| {
                let mut admitted = task.clone();
                if let Some(selectors) = admitted_selectors.get(task.id.as_str()) {
                    admitted.context_files = selectors.clone();
                }
                admitted
            })
            .collect::<Vec<_>>();
        for path in out_of_scope_paths(committed, &admitted_tasks) {
            let selector = format!("file:{}", normalize_git_path(&path));
            if !self.selectors_widened.contains(&selector) {
                self.selectors_widened.push(selector);
            }
        }
        Ok(())
    }

    /// Append `file:<path>` selectors for reviewer-changed paths no task
    /// covers to the reviewed (first) task — the task whose agent changed
    /// them — with review provenance in its history, then bind the
    /// certificate to the post-widening task-meaning digest.
    fn widen_reviewer_selectors(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        paths: &[String],
    ) -> Result<(), OrbitError> {
        let Some(task_id) = context.tasks.first().map(|task| task.id.clone()) else {
            return Ok(());
        };
        let paths = paths
            .iter()
            .map(|path| normalize_git_path(path))
            .collect::<Vec<_>>();
        if context.claimed {
            // A claim's footprint is fixed until its handoff: the owner
            // widens it then, for every path the candidate changed outside
            // it, and the certificate reports what that will add.
            for path in &paths {
                let selector = format!("file:{path}");
                if !self.selectors_widened.contains(&selector) {
                    self.selectors_widened.push(selector);
                }
            }
            return Ok(());
        }
        let widened = runtime.widen_context_files_for_paths(
            &task_id,
            &context.run_id,
            ContextWideningStep::Review,
            "review_gate_settle",
            &paths,
        )?;
        if widened.is_empty() {
            return Ok(());
        }
        context.tasks[0] = runtime.get_task(&task_id)?;
        context.refresh_task_digests()?;
        self.task_meaning_digest = context.task_digests.1.clone();
        self.selectors_widened = widened;
        Ok(())
    }

    /// Resolve a repeated evidence-only report from durable result/log pairs
    /// on the final tree, never from the admission's advisory snapshot. Pairs
    /// on an earlier tree count through `carry` only: its patch is unchanged.
    /// `host` holds the results this settlement's own host just produced
    /// ([`Self::fulfil_host_evidence`]), which count like durable ones.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconcile_external_evidence(
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
            super::super::evidence::canonical_requirements(&self.external_evidence)
        else {
            return Ok(());
        };
        self.external_evidence = requirements;
        let Some(validation) = super::super::evidence::with_external_checks_passed(
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
            },
        )
        .is_err()
        {
            return Ok(());
        }
        let mut satisfied = super::super::evidence::satisfied_external_evidence(
            runtime,
            &context.task_ids[0],
            candidate,
        )?;
        satisfied.extend(host);
        let carried = super::super::evidence::carried_external_evidence(
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

    /// Cross-check the claimed verdict against what actually happened.
    /// `scope` is what validation sources are judged against: every task
    /// selector plus the candidate's changed paths.
    pub(super) fn reconcile_verdict(&mut self, repair: Option<&CommitIdentity>, scope: &[String]) {
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
            let context = ValidationContext {
                scope,
                obligations: &self.retained_obligations,
                retired: &self.retired_validation,
                required_validation_commands: self.required_validation_commands.as_deref(),
            };
            match validation_evidence(&self.validation, &context) {
                Ok(()) => self.validation_complete = true,
                Err(defect) => self.downgrade(&defect.reason()),
            }
        }
    }

    pub(super) fn downgrade(&mut self, reason: &str) {
        self.host_refused = true;
        self.verdict = ReviewVerdict::Incomplete;
        self.external_evidence.clear();
        self.validation_complete = false;
        self.escalate(reason);
    }

    pub(super) fn escalate(&mut self, reason: &str) {
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

/// One report for the bundle: the most severe verdict, every distinct
/// finding and validation record, and every distinct summary and escalation.
fn merge_reports(reports: Vec<ReviewReport>) -> Option<ReviewReport> {
    let mut reports = reports.into_iter();
    let mut merged = reports.next()?;
    for report in reports {
        for required in report.external_evidence {
            if !merged.external_evidence.contains(&required) {
                merged.external_evidence.push(required);
            }
        }
        if verdict_severity(report.verdict) > verdict_severity(merged.verdict) {
            merged.verdict = report.verdict;
        }
        for finding in report.findings {
            if !merged.findings.contains(&finding) {
                merged.findings.push(finding);
            }
        }
        for record in report.validation {
            if !merged.validation.contains(&record) {
                merged.validation.push(record);
            }
        }
        for retirement in report.retired_validation {
            if !merged.retired_validation.contains(&retirement) {
                merged.retired_validation.push(retirement);
            }
        }
        append_distinct(&mut merged.summary, &report.summary, "\n");
        if let Some(escalation) = report.escalation {
            let current = merged.escalation.get_or_insert_with(String::new);
            append_distinct(current, &escalation, "; ");
        }
    }
    Some(merged)
}

/// The report revisions the host retained on `task_id` for this attempt,
/// other than the current report itself. A history that cannot be read is
/// an incomplete review, never an empty one.
fn retained_revisions(
    task_id: &str,
    history: Option<TaskArtifact>,
    attempt: &ReviewAttempt,
    current: &[u8],
) -> Result<Vec<ReviewReportRevision>, String> {
    let Some(history) = history else {
        return Ok(Vec::new());
    };
    let history = ReviewReportHistory::parse(&history.content).map_err(|error| {
        format!("report_history_unreadable: the report history on {task_id}: {error}")
    })?;
    let current_sha256 = sha256_hex(current);
    if history
        .for_attempt(&attempt.attempt_id)
        .find(|revision| revision.sha256 == current_sha256)
        .is_some_and(|revision| revision.record_id_contract_checked == Some(false))
    {
        let report = ReviewReport::parse(current)
            .map_err(|error| format!("report_unreadable: the report on {task_id}: {error}"))?;
        ReviewReportHistory::check_required_record_ids(&report).map_err(|error| {
            format!("report_record_ids_invalid: the post-session report on {task_id}: {error}")
        })?;
        history
            .check_record_continuity(&report)
            .map_err(|error| format!("report_record_ids_invalid: {error}"))?;
    }
    Ok(history
        .for_attempt(&attempt.attempt_id)
        .filter(|revision| revision.sha256 != current_sha256)
        .cloned()
        .collect())
}

/// The required-check records of earlier revisions, oldest first, minus any
/// the final report repeats verbatim: those add no information.
fn retained_obligations(
    revisions: Vec<ReviewReportRevision>,
    final_records: &[ReviewValidation],
) -> Vec<RetainedObligation> {
    let mut obligations: Vec<RetainedObligation> = Vec::new();
    for revision in revisions {
        for validation in revision.validation {
            if validation.role != ValidationRole::Required
                || final_records.contains(&validation)
                || obligations.iter().any(|kept| kept.validation == validation)
            {
                continue;
            }
            obligations.push(RetainedObligation {
                report_sha256: revision.sha256.clone(),
                observed_at: revision.observed_at,
                validation,
            });
        }
    }
    obligations.sort_by_key(|obligation| obligation.observed_at);
    obligations
}

fn verdict_severity(verdict: ReviewVerdict) -> u8 {
    match verdict {
        ReviewVerdict::Accept => 0,
        ReviewVerdict::AcceptWithFixes => 1,
        ReviewVerdict::Reject => 2,
        ReviewVerdict::Incomplete => 3,
    }
}

fn append_distinct(current: &mut String, addition: &str, separator: &str) {
    let addition = addition.trim();
    if addition.is_empty() || current.split(separator).any(|part| part.trim() == addition) {
        return;
    }
    if !current.trim().is_empty() {
        current.push_str(separator);
    }
    current.push_str(addition);
}

/// Whether every task still means what was admitted except for selectors
/// the reviewer added through the task API: restoring the admitted
/// selectors must reproduce the admitted digest, and the current selectors
/// must contain every admitted one.
fn selectors_only_grew(
    context: &GateContext,
    attempt: &ReviewAttempt,
    admitted_selectors: &BTreeMap<String, Vec<String>>,
) -> Result<bool, OrbitError> {
    let mut digests = Vec::with_capacity(context.tasks.len());
    for task in &context.tasks {
        let Some(admitted) = admitted_selectors.get(task.id.as_str()) else {
            return Ok(false);
        };
        if admitted
            .iter()
            .any(|selector| !task.context_files.contains(selector))
        {
            return Ok(false);
        }
        let mut restored = task.clone();
        restored.context_files = admitted.clone();
        digests.push((
            task.id.to_string(),
            task_meaning_digest(&restored).map_err(automation_error)?,
        ));
    }
    let restored_digest = combined_task_meaning_digest(&digests).map_err(automation_error)?;
    Ok(restored_digest == attempt.task_meaning_digest)
}

/// A repair path is in scope when a task selector's filesystem anchor names
/// it or a directory/legacy selector contains it. Matching uses the shared
/// selector grammar, so `symbol:<path>#<symbol>:<kind>` authorizes the
/// backing file even when `<symbol>` contains `::`.
fn path_in_scope(path: &str, tasks: &[Task]) -> bool {
    let path = path.trim_start_matches("./");
    let changed = format!("file:{path}");
    tasks.iter().any(|task| {
        task.context_files
            .iter()
            .any(|selector| overlaps(selector, &changed))
    })
}

/// Repair paths no task's selectors cover.
fn out_of_scope_paths(paths: &[String], tasks: &[Task]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| !path_in_scope(path, tasks))
        .cloned()
        .collect()
}

fn normalize_git_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

/// The author label a reviewer's repair commit is attributed to.
pub(super) fn repair_author_label(reviewer: &ReviewerIdentity) -> String {
    format!("{} / {}", reviewer.provider, reviewer.model)
}

/// The task comment a settlement posts [ORB-13989]: the verdict, every
/// finding with what the reviewer changed for it, then the evidence.
pub(super) fn verdict_comment(certificate: &ReviewCertificate) -> String {
    let assurance = certificate
        .assurance
        .map(|assurance| assurance.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let reviewer_commit = if certificate.repair_commits.is_empty() {
        "none".to_string()
    } else {
        certificate
            .repair_commits
            .iter()
            .map(|commit| {
                format!(
                    "`{}` `{}` by {}",
                    commit.commit,
                    one_line(&commit.subject),
                    commit.author
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "before-PR review settled attempt `{}`: verdict **{}** (assurance: {}).\n\n\
         {}\n\n\
         - Reviewer: crew `{}` ({} / {}){}\n\
         - Implementation: `{}` on base `{}` ({} commit(s), unchanged by review)\n\
         - Reviewer commit: {}\n\
         - Final candidate: `{}`\n\
         - Selectors widened for reviewer-changed paths: {}\n\
         - Validation on final candidate: {} record(s) [{}], complete: {}\n\
         - Owner-required checks: {}\n\
         - Not established by this review: {}\n\
         - Required checks retained from earlier report revisions: {}\n\
         - Reviewer runtime: {}s of {} min\n\
         - Escalation: {}\n\n\
         {}",
        certificate.attempt_id,
        certificate.verdict.as_str(),
        assurance,
        finding_lines(&certificate.findings),
        certificate.reviewer.crew,
        certificate.reviewer.provider,
        certificate.reviewer.model,
        if certificate.reviewer.same_model_as_implementer {
            "; same model as the implementer, reported as such"
        } else {
            ""
        },
        certificate.reviewed_candidate.commit,
        certificate.base.commit,
        certificate.implementation_commits.len(),
        reviewer_commit,
        certificate.final_candidate.commit,
        if certificate.selectors_widened.is_empty() {
            "none".to_string()
        } else {
            certificate.selectors_widened.join(", ")
        },
        certificate.validation.len(),
        validation_roles(&certificate.validation),
        certificate.validation_complete,
        required_commands_line(certificate.required_validation_commands.as_deref()),
        limitations_line(&certificate.validation),
        retained_line(certificate),
        certificate.consumed.seconds,
        certificate.budget.minutes,
        certificate.escalation.as_deref().unwrap_or("none"),
        verdict_consequence(certificate),
    )
}

/// What the verdict means for delivery, in one paragraph.
fn verdict_consequence(certificate: &ReviewCertificate) -> &'static str {
    if !certificate.baseline_red.is_empty() {
        return "Delivery waits: every failed required check fails the same way on the pinned \
                base, so the candidate did not cause it. No PR is opened; the candidate is kept \
                and the task is held in the backlog until the base passes, when a fresh review \
                judges it.";
    }
    match certificate.verdict {
        ReviewVerdict::Accept => {
            "Accepted as implemented; the PR carries the implementation commit(s) only. This \
             verdict is review evidence, not task approval or merge permission."
        }
        ReviewVerdict::AcceptWithFixes => {
            "Accepted with the reviewer's fixes as a separate commit. Owner validation and the \
             ownership check run again on that head before the PR opens; a failure there blocks \
             the task as `reject`. The fixes were validated but not independently reviewed, and \
             this verdict is not task approval or merge permission."
        }
        ReviewVerdict::Reject | ReviewVerdict::Incomplete => {
            "Delivery stops: no PR is opened, the task is blocked, and the candidate branch keeps \
             every commit for final recovery or an operator decision. There is no second review \
             round."
        }
    }
}

/// Every finding of the attempt with its disposition and, for a fix, what
/// the reviewer changed and where.
fn finding_lines(findings: &[orbit_types::workflow::ReviewFinding]) -> String {
    if findings.is_empty() {
        return "Findings: none.".to_string();
    }
    let mut lines = String::from("Findings:");
    for finding in findings {
        let disposition = match &finding.disposition {
            FindingDisposition::Open => "open".to_string(),
            FindingDisposition::Repaired => "fixed".to_string(),
            FindingDisposition::Disposed { reason } => format!("disposed: {}", one_line(reason)),
        };
        lines.push_str(&format!(
            "\n- `{}` [{}, {}] {}",
            finding.id,
            finding.severity,
            disposition,
            one_line(&finding.summary),
        ));
        if let Some(change) = finding_change(finding) {
            lines.push_str(&format!("\n  - Changed: {change}"));
        }
    }
    lines
}

/// What a fixed finding changed, with its paths; `None` for any other
/// disposition.
fn finding_change(finding: &orbit_types::workflow::ReviewFinding) -> Option<String> {
    if finding.disposition != FindingDisposition::Repaired {
        return None;
    }
    let change = finding
        .change
        .as_deref()
        .map(one_line)
        .filter(|change| !change.is_empty())
        .unwrap_or_else(|| "not described by the reviewer".to_string());
    Some(if finding.paths.is_empty() {
        change
    } else {
        format!("{change} ({})", finding.paths.join(", "))
    })
}

/// The PR body's review sections: "Review fixes" when the reviewer committed
/// fixes, listing each fixed finding and what changed, and "Review validation
/// limits" when a diagnostic failed, so the PR never reads as a claim that
/// the whole workspace passed. `None` when neither applies.
pub(super) fn review_fixes_section(certificate: &ReviewCertificate) -> Option<String> {
    let sections = [
        fixes_section(certificate),
        validation_section(certificate),
        limits_section(&certificate.validation),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    (!sections.is_empty()).then(|| sections.join("\n"))
}

fn validation_section(certificate: &ReviewCertificate) -> Option<String> {
    if certificate.validation.is_empty() && certificate.required_validation_commands.is_none() {
        return None;
    }
    let mut section = format!(
        "## Review validation\n\nOwner-required commands: {}. Validation complete: {}.\n",
        required_commands_line(certificate.required_validation_commands.as_deref()),
        certificate.validation_complete,
    );
    for record in &certificate.validation {
        section.push_str(&format!(
            "\n- `{}` — {} ({}){}{}{}",
            one_line(&record.command),
            record.outcome.as_str(),
            record.role.as_str(),
            record
                .control
                .map(|control| format!("; control: {}", control.as_str()))
                .unwrap_or_default(),
            record
                .note
                .as_deref()
                .filter(|note| !note.trim().is_empty())
                .map(|note| format!("; rationale: {}", one_line(note)))
                .unwrap_or_default(),
            if record.sources.is_empty() {
                String::new()
            } else {
                format!("; sources: {}", record.sources.join(", "))
            },
        ));
    }
    Some(section)
}

fn required_commands_line(commands: Option<&[String]>) -> String {
    match commands {
        Some([]) => "none configured".to_string(),
        Some(commands) => commands
            .iter()
            .map(|command| format!("`{}`", one_line(command)))
            .collect::<Vec<_>>()
            .join(", "),
        None => "missing legacy host contract; fresh review required".to_string(),
    }
}

fn limits_section(records: &[ReviewValidation]) -> Option<String> {
    let limitations = validation_limitations(records);
    if limitations.is_empty() {
        return None;
    }
    let mut section = String::from(
        "## Review validation limits\n\nEvery required check passed on the reviewed head. \
         These diagnostics were observed outside the task's scope, kept as observed, and are \
         not covered by this review:\n",
    );
    for limitation in limitations {
        section.push_str(&format!("\n- {}", one_line(&limitation)));
    }
    Some(section)
}

fn fixes_section(certificate: &ReviewCertificate) -> Option<String> {
    let commit = certificate.repair_commits.first()?;
    let mut section = format!(
        "## Review fixes\n\nThe before-PR reviewer (crew `{}`) fixed its findings in `{}` \
         (`{}`), a separate commit on top of the implementation. Owner validation reran on \
         that head.\n",
        certificate.reviewer.crew,
        commit.commit,
        one_line(&commit.subject),
    );
    for finding in &certificate.findings {
        if let Some(change) = finding_change(finding) {
            section.push_str(&format!(
                "\n- `{}` [{}] {} — {change}",
                finding.id,
                finding.severity,
                one_line(&finding.summary),
            ));
        }
    }
    Some(section)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The classification breakdown of a validation set, so a reader sees which
/// records were required checks and which were controls or exclusions
/// without opening the certificate.
fn validation_roles(records: &[ReviewValidation]) -> String {
    let counts = validation_role_counts(records);
    if counts.is_empty() {
        return "none".to_string();
    }
    counts
        .into_iter()
        .map(|(role, count)| format!("{count} {}", role.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The failed diagnostics a certificate does not cover, on one line.
fn limitations_line(records: &[ReviewValidation]) -> String {
    let limitations = validation_limitations(records);
    if limitations.is_empty() {
        "none".to_string()
    } else {
        limitations
            .iter()
            .map(|limitation| one_line(limitation))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Earlier report revisions' required checks, with their observed outcomes
/// and, for one the final report retired, the reason it gave.
fn retained_line(certificate: &ReviewCertificate) -> String {
    if certificate.retained_obligations.is_empty() {
        return "none".to_string();
    }
    certificate
        .retained_obligations
        .iter()
        .map(|obligation| {
            let validation = &obligation.validation;
            let id = validation.record_id();
            let retired = id.and_then(|id| {
                certificate
                    .retired_validation
                    .iter()
                    .find(|retired| retired.id.trim() == id)
            });
            format!(
                "{}`{}` {}{}",
                id.map(|id| format!("{id} ")).unwrap_or_default(),
                one_line(&validation.command),
                validation.outcome.as_str(),
                retired
                    .map(|retired| format!(" (retired: {})", one_line(&retired.reason)))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Write a gate artifact under the executor run's authority. A claimed leaf
/// owns no task state: the artifact crosses its binding to the owner as
/// claim evidence, like its validation logs [ORB-13908].
pub(super) fn write_artifact(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    path: &str,
    content: &[u8],
) -> Result<(), OrbitError> {
    if runtime.worker_invocation().is_some() {
        runtime.route_worker_tool(
            "orbit.task.artifact.put",
            serde_json::json!({
                "id": task_id,
                "artifacts": [{
                    "path": path,
                    "content": content,
                    "media_type": "application/json",
                }],
            }),
            Default::default(),
        )?;
        return Ok(());
    }
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: path.to_string(),
                content: content.to_vec(),
                media_type: "application/json".to_string(),
                // The record writer derives provenance from the write actor.
                created_by: None,
            }],
            ..TaskUpdateParams::default()
        },
        Some(run_id.to_string()),
    )?;
    Ok(())
}
