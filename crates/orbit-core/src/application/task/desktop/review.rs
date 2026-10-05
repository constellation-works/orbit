use orbit_common::OrbitError;
use orbit_types::{
    desktop::{DesktopReviewDecision, DesktopReviewVerdict},
    task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus},
    workflow::handoff::AcceptedHandoff,
};

use crate::OrbitRuntime;
use crate::application::review::reconciliation;

use super::execution::{HandoffPullRequest, pull_number, reported};
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
        match source {
            PullRequestSource::Reference { selector, url } => {
                let status = self.read_pull_request_status(&selector)?;
                let head = reported(&status, "headRefOid").ok_or_else(|| {
                    invalid(&format!("the provider reported no head for {selector}"))
                })?;
                let reported_url = reported(&status, "url");
                if url.is_some() && reported_url != url {
                    return Err(invalid(
                        "the provider answered for a different pull request than the task links",
                    ));
                }
                Ok(Some(DesktopPullRequest {
                    head,
                    url: reported_url,
                    completion_refusal: None,
                }))
            }
            PullRequestSource::Handoff { number, accepted } => {
                let pull_request = self.observe_handoff_pull_request(number, &accepted)?;
                let completion_refusal =
                    self.handoff_completion_refusal(task, &accepted, &pull_request)?;
                Ok(Some(DesktopPullRequest {
                    head: pull_request.head,
                    url: Some(pull_request.url),
                    completion_refusal,
                }))
            }
        }
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
        let foreign = self.desktop_foreign_execution(task)?;
        let handoff = foreign
            .as_ref()
            .and_then(|foreign| foreign.pull_request_handoff())
            .map(|(number, accepted)| (number, Box::new(accepted.clone())));
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
    /// neither, and completes only on an accepted review reconciliation of
    /// exactly that head.
    fn handoff_completion_refusal(
        &self,
        task: &Task,
        accepted: &AcceptedHandoff,
        pull_request: &HandoffPullRequest,
    ) -> Result<Option<String>, OrbitError> {
        if !pull_request.merged() {
            return Ok(Some(format!(
                "pull request #{} has not merged (state {}); a handed-off delivery completes \
                 once its pull request merges",
                pull_request.number, pull_request.state
            )));
        }
        if pull_request.head == accepted.handoff.candidate.candidate.commit {
            return Ok(None);
        }
        reconciliation::merged_head_completion_refusal(self, task, accepted, pull_request)
    }
}
