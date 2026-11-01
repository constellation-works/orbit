use orbit_common::OrbitError;
use orbit_types::{
    desktop::{DesktopReviewDecision, DesktopReviewVerdict},
    task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus},
    workflow::handoff::AcceptedHandoff,
};

use crate::OrbitRuntime;
use crate::application::task::TaskReviewPort;

use super::execution::{HandoffPullRequest, pull_number};
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
    /// The exact native publication, already authenticated against its provider.
    Native(DesktopPullRequest),
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
            PullRequestSource::Native(pull_request) => Ok(Some(pull_request)),
            PullRequestSource::Reference { selector, url } => {
                let status = self.read_pull_request_status(&selector)?;
                let head = status.head_sha.ok_or_else(|| {
                    invalid(&format!("the provider reported no head for {selector}"))
                })?;
                let reported_url = status.url;
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

    /// Select from authenticated execution evidence when references include
    /// history. An accepted foreign handoff needs no task PR reference.
    fn desktop_pull_request_source(
        &self,
        task: &Task,
    ) -> Result<Option<PullRequestSource>, OrbitError> {
        let refs: Vec<_> = task
            .external_refs
            .iter()
            .filter(|r| r.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .collect();
        let foreign = self.desktop_foreign_execution(task)?;
        if let Some((number, accepted)) = foreign
            .as_ref()
            .and_then(|foreign| foreign.pull_request_handoff())
        {
            let url = format!(
                "https://github.com/{}/pull/{number}",
                accepted.handoff.candidate.repository
            );
            validate_publication_references(task, number, &url, true)?;
            return Ok(Some(PullRequestSource::Handoff {
                number,
                accepted: Box::new(accepted.clone()),
            }));
        }
        if foreign.is_none() && refs.len() > 1 {
            let proof = self.desktop_native_publication_proof(task)?;
            let number = proof
                .number
                .parse::<u64>()
                .map_err(|_| invalid("native publication has no exact pull request number"))?;
            if number == 0 || proof.number != number.to_string() {
                return Err(invalid(
                    "native publication has an inexact pull request number",
                ));
            }
            let remote = git2::Repository::open(&self.paths().repo_root)
                .and_then(|repository| repository.config()?.get_string("remote.origin.url"))
                .map_err(|_| invalid("native publication repository identity is unavailable"))?;
            let repository = orbit_types::workspace::git_remote_identity(&remote)
                .map_err(|_| invalid("native publication repository identity is invalid"))?;
            let repository = repository.strip_prefix("github.com/").ok_or_else(|| {
                invalid("native publication requires an exact GitHub repository identity")
            })?;
            // The identity is lowercased for comparison; the forge's URLs keep
            // the repository's canonical case.
            let url = format!("https://github.com/{repository}/pull/{number}");
            if !proof
                .url
                .as_deref()
                .is_some_and(|proof_url| same_pull_request_url(proof_url, &url))
            {
                return Err(invalid("native publication URL and repository disagree"));
            }
            validate_publication_references(task, number, &url, false)?;
            let status = self.read_pull_request_status(&number.to_string())?;
            if status.number != Some(number)
                || !status
                    .url
                    .as_deref()
                    .is_some_and(|status_url| same_pull_request_url(status_url, &url))
                || status.head_branch.as_deref() != Some(proof.branch.as_str())
                || status.base_branch.as_deref() != Some(proof.base_branch.as_str())
                || status.head_sha.as_deref() != Some(proof.head.as_str())
            {
                return Err(invalid(
                    "the provider no longer matches the authenticated native publication",
                ));
            }
            // The provider's own URL names the reviewed pull request, so
            // evidence citing it matches the forge's canonical form.
            return Ok(Some(PullRequestSource::Native(DesktopPullRequest {
                head: proof.head,
                url: status.url,
                completion_refusal: None,
            })));
        }
        if refs.len() > 1 {
            return Err(invalid(
                "desktop completion requires authenticated current publication evidence for multiple PR references",
            ));
        }
        Ok(refs.first().map(|reference| PullRequestSource::Reference {
            selector: reference
                .url
                .clone()
                .unwrap_or_else(|| reference.id.clone()),
            url: reference.url.clone(),
        }))
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
        self.merged_head_reconciliation_refusal(task, accepted, pull_request)
    }
}

/// References corroborate authenticated delivery identity; their order has no
/// authority. Keep historical references while refusing contradictory aliases.
fn validate_publication_references(
    task: &Task,
    number: u64,
    url: &str,
    allow_absent: bool,
) -> Result<(), OrbitError> {
    let mut associated = false;
    for reference in task
        .external_refs
        .iter()
        .filter(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
    {
        if reference.id == number.to_string() {
            if reference
                .url
                .as_deref()
                .is_some_and(|reference_url| !same_pull_request_url(reference_url, url))
            {
                return Err(invalid(
                    "the task's PR reference contradicts its authenticated publication URL",
                ));
            }
            associated = true;
        } else if reference
            .url
            .as_deref()
            .is_some_and(|reference_url| pull_number(reference_url) == Some(number))
        {
            return Err(invalid(
                "the task's PR reference contradicts its authenticated publication number",
            ));
        }
    }
    if !associated && !allow_absent {
        return Err(invalid(
            "the task's PR references do not identify its authenticated publication",
        ));
    }
    Ok(())
}

/// Two pull-request URLs name the same pull request when they differ only in
/// ASCII case: GitHub owner and repository names are case-insensitive, and the
/// forge reports them in canonical case.
fn same_pull_request_url(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}
