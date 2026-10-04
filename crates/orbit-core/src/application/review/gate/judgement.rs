//! Check the reviewer's claims against the repository, the task scope and
//! the budget, and render the verdict comment.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_automation::review::{
    combined_task_meaning_digest, task_meaning_digest, validation_evidence, validation_role_counts,
};
use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_engine::review_gate::{
    REVIEW_ATTEMPT_TRAILER, commit_reviewer_repairs, uncommitted_paths,
};
use orbit_types::task::{Task, TaskArtifact};
use orbit_types::workflow::{
    CommitIdentity, FindingDisposition, REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT,
    ReviewAttempt, ReviewCertificate, ReviewLedger, ReviewReport, ReviewVerdict, ReviewerIdentity,
};

use super::super::automation_error;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::context::GateContext;

/// The reviewer's claims, checked against the repository and the budget.
pub(super) struct Judgement {
    pub(super) verdict: ReviewVerdict,
    pub(super) findings: Vec<orbit_types::workflow::ReviewFinding>,
    pub(super) validation: Vec<orbit_types::workflow::ReviewValidation>,
    pub(super) validation_complete: bool,
    pub(super) escalation: Option<String>,
    summary: String,
    pub(super) task_meaning_digest: String,
    pub(super) selectors_widened: Vec<String>,
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
            verdict: ReviewVerdict::Incomplete,
            findings: Vec::new(),
            validation: Vec::new(),
            validation_complete: false,
            escalation: Some(reason.to_string()),
            summary: String::new(),
            task_meaning_digest: task_meaning_digest.clone(),
            selectors_widened: Vec::new(),
        };
        let mut reports = Vec::new();
        let mut stale = false;
        for task_id in &context.task_ids {
            let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_REPORT_ARTIFACT)? else {
                continue;
            };
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
        Ok(Self {
            verdict: report.verdict,
            findings: report.findings,
            validation: report.validation,
            validation_complete: false,
            escalation: report.escalation,
            summary: report.summary,
            task_meaning_digest,
            selectors_widened: Vec::new(),
        })
    }

    /// Task criteria, scope, or contract changes during the review
    /// invalidate it. A reviewer may add selectors for a coupled repair
    /// through the task API; anything else re-establishes review.
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

    /// Commit whatever the reviewer changed as its own attributed work.
    ///
    /// An out-of-selector path listed on a repaired finding is a declared
    /// coupled repair: the gate widens `context_files` the same way the
    /// reviewer already may, and does not abandon the review. A changed
    /// path named by no finding stays a silent drive-by and still
    /// downgrades.
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
        let (declared_out_of_scope, undeclared) = self.classify_repairs(&changed, &context.tasks);
        if !declared_out_of_scope.is_empty() {
            self.widen_declared_selectors(runtime, context, &declared_out_of_scope)?;
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
        if !undeclared.is_empty() {
            self.downgrade(&undeclared_repair_reason(&undeclared));
        }
        Ok(commit)
    }

    /// Adopt the repair commit an interrupted settlement of this attempt
    /// already made, judging its paths exactly as [`Self::commit_repairs`]
    /// judged them before committing: an undeclared out-of-scope path still
    /// downgrades. The interrupted run widened selectors before committing,
    /// so a declared repair path outside the admitted selectors is reported
    /// as widened even though it is in scope by now.
    pub(super) fn adopt_repairs(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        committed: &[String],
        admitted_selectors: &BTreeMap<String, Vec<String>>,
    ) -> Result<(), OrbitError> {
        let (declared_out_of_scope, undeclared) = self.classify_repairs(committed, &context.tasks);
        if !declared_out_of_scope.is_empty() {
            self.widen_declared_selectors(runtime, context, &declared_out_of_scope)?;
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
        let (widened_since_admission, _) = self.classify_repairs(committed, &admitted_tasks);
        for path in widened_since_admission {
            let selector = format!("file:{}", normalize_git_path(&path));
            if !self.selectors_widened.contains(&selector) {
                self.selectors_widened.push(selector);
            }
        }
        if !undeclared.is_empty() {
            self.downgrade(&undeclared_repair_reason(&undeclared));
        }
        Ok(())
    }

    /// Split repair paths outside every task's scope into those a repaired
    /// finding declared and those no finding named.
    fn classify_repairs(&self, paths: &[String], tasks: &[Task]) -> (Vec<String>, Vec<String>) {
        let declared = declared_repair_paths(&self.findings);
        let mut declared_out_of_scope = Vec::new();
        let mut undeclared = Vec::new();
        for path in paths {
            if path_in_scope(path, tasks) {
                continue;
            }
            if declared.contains(&normalize_git_path(path)) {
                declared_out_of_scope.push(path.clone());
            } else {
                undeclared.push(path.clone());
            }
        }
        (declared_out_of_scope, undeclared)
    }

    /// Append `file:<path>` selectors for declared coupled repairs, then bind
    /// the certificate to the post-widening task-meaning digest.
    fn widen_declared_selectors(
        &mut self,
        runtime: &OrbitRuntime,
        context: &mut GateContext,
        paths: &[String],
    ) -> Result<(), OrbitError> {
        let mut widened = Vec::new();
        let mut updated = vec![false; context.tasks.len()];
        for path in paths {
            let selector = format!("file:{}", normalize_git_path(path));
            for (index, task) in context.tasks.iter_mut().enumerate() {
                if path_in_scope(path, std::slice::from_ref(task))
                    || task
                        .context_files
                        .iter()
                        .any(|existing| existing == &selector)
                {
                    continue;
                }
                task.context_files.push(selector.clone());
                updated[index] = true;
                if !widened.contains(&selector) {
                    widened.push(selector.clone());
                }
            }
        }
        if widened.is_empty() {
            return Ok(());
        }
        for (task, updated) in context.tasks.iter().zip(updated) {
            if updated {
                runtime.update_task_as_system(
                    &task.id,
                    TaskUpdateParams {
                        context_files: Some(task.context_files.clone()),
                        ..TaskUpdateParams::default()
                    },
                    None,
                )?;
            }
        }
        context.refresh_task_digests()?;
        self.task_meaning_digest = context.task_digests.1.clone();
        self.selectors_widened = widened;
        Ok(())
    }

    /// Cross-check the claimed verdict against what actually happened.
    pub(super) fn reconcile_verdict(
        &mut self,
        ledger: &ReviewLedger,
        repair: Option<&CommitIdentity>,
    ) {
        let open_findings = open_findings(&self.findings).count();
        match self.verdict {
            ReviewVerdict::PassedWithoutRepairs if repair.is_some() => self.downgrade(
                "verdict_inconsistent: the reviewer reported no repairs but changed the worktree",
            ),
            ReviewVerdict::PassedWithRepairs if repair.is_none() => self.downgrade(
                "verdict_inconsistent: the reviewer reported repairs but changed nothing",
            ),
            ReviewVerdict::PassedWithRepairs if ledger.remaining().repair_cycles == 0 => self
                .downgrade(
                    "review_repair_cycles_exhausted: the lineage has no repair cycle left for \
                     these reviewer repairs",
                ),
            ReviewVerdict::PassedWithoutRepairs | ReviewVerdict::PassedWithRepairs
                if open_findings > 0 =>
            {
                self.downgrade(&format!(
                    "verdict_inconsistent: {open_findings} finding(s) remain open under a pass"
                ));
            }
            ReviewVerdict::ChangesRequired if self.escalation.is_none() => {
                self.escalation = Some("changes_required".to_string());
            }
            _ => {}
        }
        // A pass rests on what the records establish, not on their count:
        // a required check must have passed, while a declared negative
        // control, an excluded action, and a superseded attempt carry their
        // own consistency rules. Delivery coverage reads the same function.
        if self.verdict.passed() {
            match validation_evidence(&self.validation) {
                Ok(()) => self.validation_complete = true,
                Err(defect) => self.downgrade(&defect.reason()),
            }
        }
    }

    /// Whether a settled `changes_required` verdict goes back to the
    /// implementer for rework within this run [ORB-13891].
    ///
    /// Rework needs an open finding to act on, a reviewer start left for the
    /// re-review, a repair cycle left after this attempt's own reviewer
    /// repair, and reviewer minutes left. `ledger` is the lineage while this
    /// attempt was still open, so a replayed settlement decides exactly as
    /// the first one did. A refusal is recorded in the escalation, and
    /// delivery stops as for any other non-pass verdict.
    pub(super) fn request_rework(
        &mut self,
        ledger: &ReviewLedger,
        reviewer_repair_cycles: u32,
        now: DateTime<Utc>,
    ) -> bool {
        if self.verdict != ReviewVerdict::ChangesRequired {
            return false;
        }
        let remaining = ledger.remaining_at(now);
        let budget = ledger.budget;
        let refusal = if open_findings(&self.findings).next().is_none() {
            "review_rework_unavailable: changes_required names no open finding for the \
             implementer to rework"
                .to_string()
        } else if remaining.reviewer_starts == 0 {
            format!(
                "review_rework_exhausted: review_starts_exhausted: all {} reviewer start(s) of \
                 the lineage are spent, so a rework could not be re-reviewed",
                budget.reviewer_starts
            )
        } else if remaining.repair_cycles <= reviewer_repair_cycles {
            format!(
                "review_rework_exhausted: review_repair_cycles_exhausted: all {} repair \
                 cycle(s) of the lineage are spent",
                budget.repair_cycles
            )
        } else if remaining.seconds == 0 {
            format!(
                "review_rework_exhausted: review_minutes_exhausted: the lineage's {} reviewer \
                 minute(s) are spent",
                budget.minutes
            )
        } else {
            return true;
        };
        self.escalate(&refusal);
        false
    }

    fn downgrade(&mut self, reason: &str) {
        self.verdict = ReviewVerdict::Incomplete;
        self.validation_complete = false;
        self.escalate(reason);
    }

    fn escalate(&mut self, reason: &str) {
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
        append_distinct(&mut merged.summary, &report.summary, "\n");
        if let Some(escalation) = report.escalation {
            let current = merged.escalation.get_or_insert_with(String::new);
            append_distinct(current, &escalation, "; ");
        }
    }
    Some(merged)
}

fn verdict_severity(verdict: ReviewVerdict) -> u8 {
    match verdict {
        ReviewVerdict::PassedWithoutRepairs => 0,
        ReviewVerdict::PassedWithRepairs => 1,
        ReviewVerdict::ChangesRequired => 2,
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

/// Git-relative paths listed on findings the reviewer marked repaired.
fn declared_repair_paths(findings: &[orbit_types::workflow::ReviewFinding]) -> BTreeSet<String> {
    findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Repaired)
        .flat_map(|finding| finding.paths.iter())
        .map(|path| normalize_finding_path(path))
        .filter(|path| !path.is_empty())
        .collect()
}

fn normalize_git_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

fn normalize_finding_path(path: &str) -> String {
    let trimmed = path.trim().trim_start_matches("./");
    trimmed
        .strip_prefix("file:")
        .unwrap_or(trimmed)
        .trim()
        .trim_start_matches("./")
        .to_string()
}

fn undeclared_repair_reason(paths: &[String]) -> String {
    let listed = paths
        .iter()
        .map(|path| normalize_git_path(path))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() == 1 {
        format!("repair_out_of_scope: {listed} was changed but named by no finding")
    } else {
        format!("repair_out_of_scope: {listed} were changed but named by no finding")
    }
}

/// The author label a reviewer's repair commit is attributed to.
pub(super) fn repair_author_label(reviewer: &ReviewerIdentity) -> String {
    format!("{} / {}", reviewer.provider, reviewer.model)
}

pub(super) fn verdict_comment(certificate: &ReviewCertificate) -> String {
    let assurance = certificate
        .assurance
        .map(|assurance| assurance.as_str().to_string())
        .unwrap_or_else(|| "none".to_string());
    let repairs = if certificate.repair_commits.is_empty() {
        "none".to_string()
    } else {
        certificate
            .repair_commits
            .iter()
            .map(|commit| format!("`{}` by {}", commit.commit, commit.author))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "before-PR review gate settled attempt `{}`: verdict **{}** (assurance: {}).\n\n\
         - Reviewer: crew `{}` ({} / {}){}\n\
         - Reviewed candidate: `{}` on base `{}` ({} implementation commit(s))\n\
         - Final candidate: `{}`\n\
         - Reviewer repair commits: {}\n\
         - Selectors widened from repaired findings: {}\n\
         - Findings: {} ({} open)\n\
         - Validation on final candidate: {} record(s) [{}], complete: {}\n\
         - Consumed: {} reviewer start(s), {} repair cycle(s), {}s of {} min\n\
         - Escalation: {}{}{}\n\n\
         Reviewer repairs were validated but not independently reviewed; this verdict is \
         review evidence, not task approval or merge permission.",
        certificate.attempt_id,
        certificate.verdict.as_str(),
        assurance,
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
        certificate.final_candidate.commit,
        repairs,
        if certificate.selectors_widened.is_empty() {
            "none".to_string()
        } else {
            certificate.selectors_widened.join(", ")
        },
        certificate.findings.len(),
        open_findings(&certificate.findings).count(),
        certificate.validation.len(),
        validation_roles(&certificate.validation),
        certificate.validation_complete,
        certificate.consumed.reviewer_starts,
        certificate.consumed.repair_cycles,
        certificate.consumed.seconds,
        certificate.budget.minutes,
        certificate.escalation.as_deref().unwrap_or("none"),
        rework_line(certificate),
        finding_lines(&certificate.findings),
    )
}

/// The rework decision a `changes_required` verdict carries [ORB-13891].
fn rework_line(certificate: &ReviewCertificate) -> &'static str {
    match (certificate.verdict, certificate.rework_requested) {
        (ReviewVerdict::ChangesRequired, true) => {
            "\n- Rework: requested; the implementer addresses the open findings in this run and \
             the new head is reviewed again"
        }
        (ReviewVerdict::ChangesRequired, false) => {
            "\n- Rework: not requested; delivery stops for a recorded decision"
        }
        _ => "",
    }
}

/// Every finding of the attempt, one line each, so the task keeps each
/// review cycle's findings even after a later attempt replaces the
/// certificate artifact.
fn finding_lines(findings: &[orbit_types::workflow::ReviewFinding]) -> String {
    let mut lines = String::new();
    for finding in findings {
        let disposition = match &finding.disposition {
            FindingDisposition::Open => "open".to_string(),
            FindingDisposition::Repaired => "repaired".to_string(),
            FindingDisposition::Disposed { reason } => format!("disposed: {}", one_line(reason)),
        };
        let paths = if finding.paths.is_empty() {
            String::new()
        } else {
            format!(" ({})", finding.paths.join(", "))
        };
        lines.push_str(&format!(
            "\n  - `{}` [{}, {}] {}{}",
            finding.id,
            finding.severity,
            disposition,
            one_line(&finding.summary),
            paths
        ));
    }
    if lines.is_empty() {
        return lines;
    }
    format!("\n\nFindings:{lines}")
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The classification breakdown of a validation set, so a reader sees which
/// records were required checks and which were controls or exclusions
/// without opening the certificate.
fn validation_roles(records: &[orbit_types::workflow::ReviewValidation]) -> String {
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

/// Write a gate artifact under the executor run's authority.
pub(super) fn write_artifact(
    runtime: &OrbitRuntime,
    task_id: &str,
    run_id: &str,
    path: &str,
    content: &[u8],
) -> Result<(), OrbitError> {
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
