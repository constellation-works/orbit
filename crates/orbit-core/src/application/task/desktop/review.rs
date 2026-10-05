use orbit_common::OrbitError;
use orbit_engine::RuntimeHost;
use orbit_types::{
    desktop::{DesktopReviewDecision, DesktopReviewVerdict},
    task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus},
    workflow::{
        REVIEW_GATE_ARTIFACT, ReviewCertificate,
        handoff::{AcceptedHandoff, HandoffDelivery},
    },
};
use serde_json::Value;

use crate::OrbitRuntime;

use super::validation::{bounded_text, criteria, invalid};

/// The pull request a desktop review binds to, read from the provider by its
/// exact identity.
pub(super) struct DesktopPullRequest {
    /// The head the provider reports now.
    pub(super) head: String,
    pub(super) url: Option<String>,
    /// Why this head cannot complete the task, when it cannot. Reviewing it
    /// is still allowed.
    pub(super) completion_refusal: Option<String>,
}

/// Where the pull request's identity comes from.
enum PullRequestSource {
    /// The task's own PR reference: a URL, or a bare number.
    Reference {
        selector: String,
        url: Option<String>,
    },
    /// The handoff this owner accepted from the linked foreign run.
    Handoff {
        number: u64,
        accepted: Box<AcceptedHandoff>,
    },
}

impl OrbitRuntime {
    pub(super) fn desktop_validate_verdict(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
        pull_request: Option<&DesktopPullRequest>,
    ) -> Result<(), OrbitError> {
        if task.status != TaskStatus::Review {
            return Err(invalid("review decisions require review state"));
        }
        bounded_text(&verdict.rationale, true)?;
        if task.job_run_id != verdict.expected_run_id {
            return Err(invalid("reviewed run binding changed"));
        }
        criteria(&task.acceptance_criteria)?;
        if verdict.criteria.len() != task.acceptance_criteria.len()
            || verdict.evidence.is_empty()
            || verdict.evidence.len() > 100
        {
            return Err(invalid(
                "review verdict must cover every criterion and cite current evidence",
            ));
        }
        let manifest = self.get_task_artifact_manifest(&task.id)?;
        let known = |e: &str| {
            e == "execution_summary" && !task.execution_summary.trim().is_empty()
                || manifest.iter().any(|a| a.path == e)
                || task
                    .external_refs
                    .iter()
                    .any(|r| r.url.as_deref() == Some(e))
                || pull_request.is_some_and(|pr| pr.url.as_deref() == Some(e))
                || task.job_run_id.as_deref() == Some(e)
        };
        for e in &verdict.evidence {
            if !known(e) {
                return Err(invalid(
                    "review evidence must reference current summary, run, artifact or external reference",
                ));
            }
        }
        for (expected, actual) in task.acceptance_criteria.iter().zip(&verdict.criteria) {
            if &actual.criterion != expected
                || actual.evidence.is_empty()
                || actual.evidence.len() > 100
                || actual.evidence.iter().any(|e| !known(e))
                || (verdict.decision == DesktopReviewDecision::Accept && !actual.met)
            {
                return Err(invalid(
                    "criterion outcome is missing, unmet, or references unavailable evidence",
                ));
            }
        }
        Ok(())
    }
    pub(super) fn desktop_observe_pr_head(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
    ) -> Result<Option<DesktopPullRequest>, OrbitError> {
        let pull_request = self.desktop_current_pull_request(task)?;
        if verdict.expected_head.as_deref() != pull_request.as_ref().map(|pr| pr.head.as_str()) {
            return Err(invalid("linked PR head changed since review"));
        }
        Ok(pull_request)
    }
    /// The task's pull request as the provider reports it now, looked up by
    /// its exact identity rather than found in a bounded recent-PR listing.
    pub(super) fn desktop_current_pull_request(
        &self,
        task: &Task,
    ) -> Result<Option<DesktopPullRequest>, OrbitError> {
        let Some(source) = self.desktop_pull_request_source(task)? else {
            return Ok(None);
        };
        let selector = match &source {
            PullRequestSource::Reference { selector, .. } => selector.clone(),
            PullRequestSource::Handoff { number, .. } => number.to_string(),
        };
        let response = self
            .run_private_vcs_operation(
                "pr.status",
                serde_json::json!({
                    "pr": selector,
                    "workspace_path": self.paths().repo_root.to_string_lossy(),
                }),
            )
            .map_err(|error| {
                invalid(&format!(
                    "pull request {selector} could not be read from the provider: {error}"
                ))
            })?;
        let status = &response["pull_request"];
        let head = reported(status, "headRefOid")
            .ok_or_else(|| invalid(&format!("the provider reported no head for {selector}")))?;
        let url = reported(status, "url");
        let completion_refusal = match &source {
            PullRequestSource::Reference { url: expected, .. } => {
                if expected.is_some() && url != *expected {
                    return Err(invalid(
                        "the provider answered for a different pull request than the task links",
                    ));
                }
                None
            }
            PullRequestSource::Handoff { number, accepted } => {
                self.handoff_completion_refusal(task, *number, accepted, status, &head)?
            }
        };
        Ok(Some(DesktopPullRequest {
            head,
            url,
            completion_refusal,
        }))
    }

    /// The task's PR reference and the accepted handoff's delivery must name
    /// one pull request; either alone is enough.
    fn desktop_pull_request_source(
        &self,
        task: &Task,
    ) -> Result<Option<PullRequestSource>, OrbitError> {
        let refs: Vec<_> = task
            .external_refs
            .iter()
            .filter(|r| r.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .collect();
        if refs.len() > 1 {
            return Err(invalid(
                "desktop completion requires one unambiguous PR reference",
            ));
        }
        let handoff = self
            .desktop_foreign_execution(task)?
            .and_then(|foreign| foreign.handoff)
            .and_then(|accepted| match accepted.handoff.candidate.delivery {
                HandoffDelivery::PullRequest { number } => Some((number, Box::new(accepted))),
                HandoffDelivery::LocalCandidate | HandoffDelivery::AlreadyLanded { .. } => None,
            });
        match (refs.first(), handoff) {
            (Some(reference), Some((number, _)))
                if reference.id != number.to_string()
                    || reference
                        .url
                        .as_deref()
                        .is_some_and(|url| pull_number(url) != Some(number)) =>
            {
                Err(invalid(
                    "the task's PR reference and its accepted handoff name different pull requests",
                ))
            }
            (_, Some((number, accepted))) => {
                Ok(Some(PullRequestSource::Handoff { number, accepted }))
            }
            (Some(reference), None) => Ok(Some(PullRequestSource::Reference {
                selector: reference
                    .url
                    .clone()
                    .unwrap_or_else(|| reference.id.clone()),
                url: reference.url.clone(),
            })),
            (None, None) => Ok(None),
        }
    }

    /// A handed-off delivery completes only once its own pull request merged
    /// into the landing branch, and only with evidence for the head that
    /// merged. The handoff's validation and review bind to its candidate; a
    /// head changed after handoff (a base merge, a manual fix) inherits
    /// neither.
    fn handoff_completion_refusal(
        &self,
        task: &Task,
        number: u64,
        accepted: &AcceptedHandoff,
        status: &Value,
        head: &str,
    ) -> Result<Option<String>, OrbitError> {
        let candidate = &accepted.handoff.candidate;
        let url = reported(status, "url");
        let repository = url.as_deref().and_then(pull_repository);
        if status["number"].as_u64() != Some(number)
            || url.as_deref().and_then(pull_number) != Some(number)
            || !repository.is_some_and(|slug| slug.eq_ignore_ascii_case(&candidate.repository))
        {
            return Err(invalid(&format!(
                "the provider's answer for pull request #{number} does not identify the handed-off \
                 delivery in {}",
                candidate.repository
            )));
        }
        if reported(status, "baseRefName").as_deref() != Some(candidate.landing_branch.as_str()) {
            return Err(invalid(&format!(
                "pull request #{number} no longer targets the handoff's landing branch {}",
                candidate.landing_branch
            )));
        }
        let state = reported(status, "state").unwrap_or_default();
        if !state.eq_ignore_ascii_case("MERGED") {
            return Ok(Some(format!(
                "pull request #{number} has not merged (state {state}); a handed-off delivery \
                 completes once its pull request merges"
            )));
        }
        if head == candidate.candidate.commit || self.final_head_certified(task, head)? {
            return Ok(None);
        }
        Ok(Some(format!(
            "pull request #{number} merged head {head}, not the candidate {} that run {} \
             handed off; that candidate's validation and review do not carry to a changed head. \
             Run the review gate on {head} so this task holds a passed review certificate with \
             complete validation for it, then complete",
            candidate.candidate.commit, accepted.handoff.run_id
        )))
    }

    /// Whether this task's review certificate is one the owner's review store
    /// recorded, passed with complete validation, and binds to `head`.
    fn final_head_certified(&self, task: &Task, head: &str) -> Result<bool, OrbitError> {
        let Some(artifact) = self.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)? else {
            return Ok(false);
        };
        let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
            return Ok(false);
        };
        let recorded = self
            .review_store()?
            .review_certificate(&self.workspace_id()?, &certificate.attempt_id)?;
        Ok(recorded.as_ref() == Some(&certificate)
            && certificate.task_ids.contains(&task.id.to_string())
            && certificate.verdict.passed()
            && certificate.validation_complete
            && certificate.final_candidate.commit == head)
    }
}

fn reported(status: &Value, field: &str) -> Option<String> {
    status[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `https://github.com/<owner>/<repo>/pull/<n>` → `n`.
fn pull_number(url: &str) -> Option<u64> {
    let (_, number) = url.trim_end_matches('/').rsplit_once("/pull/")?;
    number.parse().ok()
}

/// `https://github.com/<owner>/<repo>/pull/<n>` → `<owner>/<repo>`.
fn pull_repository(url: &str) -> Option<&str> {
    let (prefix, _) = url.rsplit_once("/pull/")?;
    let path = prefix.strip_prefix("https://github.com/")?;
    (path.split('/').count() == 2).then_some(path)
}
