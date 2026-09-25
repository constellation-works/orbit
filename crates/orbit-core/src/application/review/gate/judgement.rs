//! Check the reviewer's claims against the repository, the task scope and
//! the budget, and render the verdict comment.

use std::collections::{BTreeMap, BTreeSet};

use orbit_automation::review::{
    combined_task_meaning_digest, task_meaning_digest, validation_evidence, validation_role_counts,
};
use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_engine::review_gate::{CandidateIdentity, commit_reviewer_repairs, uncommitted_paths};
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
    /// Read the report the reviewer persisted for this attempt. A missing,
    /// stale, or unreadable report is an incomplete review, never a pass.
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
        let first_task = &context.task_ids[0];
        let Some(artifact) = runtime.get_task_artifact(first_task, REVIEW_REPORT_ARTIFACT)? else {
            return Ok(incomplete(
                "report_missing: the reviewer persisted no review-report.json",
            ));
        };
        let manifest = runtime.get_task_artifact_manifest(first_task)?;
        let provenance = manifest
            .iter()
            .find(|file| file.path == REVIEW_REPORT_ARTIFACT);
        if provenance.is_some_and(|file| file.created_at < attempt.started_at) {
            return Ok(incomplete(
                "report_stale: review-report.json predates this attempt",
            ));
        }
        let report: ReviewReport = match serde_json::from_slice(&artifact.content) {
            Ok(report) => report,
            Err(error) => {
                return Ok(incomplete(&format!("report_unreadable: {error}")));
            }
        };
        if report.schema_version != REVIEW_CONTRACT_VERSION {
            return Ok(incomplete(&format!(
                "report_contract_mismatch: schema_version {} is not {REVIEW_CONTRACT_VERSION}",
                report.schema_version
            )));
        }
        if report.attempt_id != attempt.attempt_id {
            return Ok(incomplete(&format!(
                "report_attempt_mismatch: report names attempt {} but {} was admitted",
                report.attempt_id, attempt.attempt_id
            )));
        }
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
        let declared = declared_repair_paths(&self.findings);
        let mut declared_out_of_scope = Vec::new();
        let mut undeclared = Vec::new();
        for path in &changed {
            if path_in_scope(path, &context.tasks) {
                continue;
            }
            if declared.contains(&normalize_git_path(path)) {
                declared_out_of_scope.push(path.clone());
            } else {
                undeclared.push(path.clone());
            }
        }
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
            "review: {} [{}]\n\nFindings: {}\nPaths: {}\nOrbit-Review-Attempt: {}\nOrbit-Review-Crew: {}",
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
        let reviewer_label = format!("{} / {}", reviewer.provider, reviewer.model);
        let commit = commit_reviewer_repairs(&context.workspace_path, &reviewer_label, &message)?;
        if !undeclared.is_empty() {
            self.downgrade(&undeclared_repair_reason(&undeclared));
        }
        Ok(commit)
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
                runtime.update_task(
                    &task.id,
                    TaskUpdateParams {
                        context_files: Some(task.context_files.clone()),
                        ..TaskUpdateParams::default()
                    },
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
        let open_findings = self
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Open)
            .count();
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

    /// A pass may not spend more wall time than the captured lineage budget,
    /// including this attempt's elapsed seconds. Non-pass verdicts still
    /// record the honest elapsed time.
    pub(super) fn enforce_wall_time(&mut self, ledger: &ReviewLedger, elapsed_seconds: u64) {
        if !self.verdict.passed() {
            return;
        }
        let budget_seconds = u64::from(ledger.budget.minutes).saturating_mul(60);
        if ledger.consumed_seconds.saturating_add(elapsed_seconds) > budget_seconds {
            self.downgrade(
                "review_minutes_exhausted: this attempt exceeded the captured lineage \
                 wall-time allowance",
            );
        }
    }

    fn downgrade(&mut self, reason: &str) {
        self.verdict = ReviewVerdict::Incomplete;
        self.validation_complete = false;
        self.escalation = Some(match self.escalation.take() {
            Some(existing) if !existing.is_empty() => format!("{existing}; {reason}"),
            _ => reason.to_string(),
        });
    }
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

pub(super) fn verdict_comment(
    certificate: &ReviewCertificate,
    reviewed: &CandidateIdentity,
) -> String {
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
         - Escalation: {}\n\n\
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
        reviewed.head.commit,
        reviewed.base.commit,
        reviewed.commits.len(),
        certificate.final_candidate.commit,
        repairs,
        if certificate.selectors_widened.is_empty() {
            "none".to_string()
        } else {
            certificate.selectors_widened.join(", ")
        },
        certificate.findings.len(),
        certificate
            .findings
            .iter()
            .filter(|finding| finding.disposition == FindingDisposition::Open)
            .count(),
        certificate.validation.len(),
        validation_roles(&certificate.validation),
        certificate.validation_complete,
        certificate.consumed.reviewer_starts,
        certificate.consumed.repair_cycles,
        certificate.consumed.seconds,
        certificate.budget.minutes,
        certificate.escalation.as_deref().unwrap_or("none"),
    )
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
    runtime.update_task_with_owner(
        task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: path.to_string(),
                content: content.to_vec(),
                media_type: "application/json".to_string(),
                created_by: Some("system".to_string()),
            }],
            ..TaskUpdateParams::default()
        },
        None,
        None,
        None,
        Some(run_id.to_string()),
    )?;
    Ok(())
}
