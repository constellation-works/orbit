//! The provider's pull-request answers, parsed once where each private PR
//! operation's response is read.
//!
//! `operations/pr.rs` produces these responses from `gh` and the private
//! transport carries them as JSON. This module is the one reader of that
//! JSON: every decision takes the typed value, so the provider's field names
//! appear here and in the producer only. Unknown fields are ignored.

use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::operations;

/// One pull request as `pr.status` (or, with fewer fields, `pr.view`)
/// reports it.
///
/// Identity and evidence text is trimmed to preserve the existing readers.
/// State tokens and the few exact comparisons retain provider whitespace;
/// normalizing those would change merge authorization or output.
#[derive(Clone, Debug)]
pub struct PullRequestStatus {
    /// The pull request number.
    pub number: Option<u64>,
    /// The engine historically also accepts signed and string PR selectors.
    pub(in crate::executor::automation::vcs) number_selector: Option<String>,
    /// The pull request's web URL.
    pub url: Option<String>,
    /// The provider's state: `OPEN`, `CLOSED` or `MERGED`.
    pub state: Option<String>,
    /// The head branch name.
    pub head_branch: Option<String>,
    /// The head commit.
    pub head_sha: Option<String>,
    /// The base branch name.
    pub base_branch: Option<String>,
    /// The commit a merged pull request landed as.
    pub merge_commit: Option<String>,
    /// Exact strings used by the original lease, supersede and merge readers.
    pub(in crate::executor::automation::vcs) exact_head_branch: Option<String>,
    pub(in crate::executor::automation::vcs) exact_head_sha: Option<String>,
    pub(in crate::executor::automation::vcs) exact_merge_commit: Option<String>,
    pub(crate) merged_at: Option<String>,
    pub(crate) merge_state_status: Option<String>,
    pub(crate) review_decision: ReviewDecision,
    /// `None` when the provider reported no readable rollup.
    pub(crate) status_checks: Option<Vec<StatusCheck>>,
    /// The provider's answer as received, kept only so landing
    /// reconciliation evidence can quote it byte for byte. No decision may
    /// read it.
    raw: Value,
}

impl PullRequestStatus {
    /// Parse a `pr.status` response. A response without `pull_request`
    /// reads as a pull request that reported nothing.
    pub fn from_status_response(response: &Value) -> Self {
        Self::parse(response.get("pull_request").cloned().unwrap_or(Value::Null))
    }

    /// Parse a `pr.view` response, which must carry `pull_request`.
    pub(in crate::executor::automation::vcs) fn from_view_response(
        response: &Value,
    ) -> Result<Self, OrbitError> {
        let pull_request = response.get("pull_request").ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS PR view did not return pull_request metadata".to_string(),
            )
        })?;
        Ok(Self::parse(pull_request.clone()))
    }

    fn parse(pull_request: Value) -> Self {
        let text = |field: &str| reported(pull_request.get(field));
        let exact = |field: &str| {
            pull_request
                .get(field)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        };
        Self {
            number: pull_request.get("number").and_then(Value::as_u64),
            number_selector: pull_request.get("number").and_then(number_selector),
            url: text("url"),
            state: exact("state"),
            head_branch: text("headRefName"),
            head_sha: text("headRefOid"),
            base_branch: text("baseRefName"),
            merge_commit: reported(pull_request.pointer("/mergeCommit/oid")),
            merged_at: text("mergedAt"),
            merge_state_status: exact("mergeStateStatus"),
            exact_head_branch: exact("headRefName"),
            exact_head_sha: exact("headRefOid"),
            exact_merge_commit: pull_request
                .pointer("/mergeCommit/oid")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            review_decision: ReviewDecision::parse(pull_request.get("reviewDecision")),
            status_checks: pull_request
                .get("statusCheckRollup")
                .and_then(Value::as_array)
                .map(|checks| checks.iter().map(StatusCheck::parse).collect()),
            raw: pull_request,
        }
    }

    /// The provider's answer exactly as received, for evidence only.
    pub(in crate::executor::automation::vcs) fn provider_state(&self) -> &Value {
        &self.raw
    }
}

/// The review decision a pull request reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReviewDecision {
    /// No review decision applies. The GraphQL API reports null, and
    /// `gh pr view --json` serializes that same null as an empty string
    /// [ORB-13759].
    NotApplicable,
    /// A decision such as `APPROVED` or `REVIEW_REQUIRED`.
    Reported(String),
    /// The field is absent or not a string.
    Unavailable,
}

impl ReviewDecision {
    fn parse(value: Option<&Value>) -> Self {
        match value {
            Some(Value::Null) => Self::NotApplicable,
            Some(Value::String(decision)) if decision.is_empty() => Self::NotApplicable,
            Some(Value::String(decision)) => Self::Reported(decision.clone()),
            _ => Self::Unavailable,
        }
    }
}

/// One entry of a pull request's status check rollup: a CheckRun
/// (`status`/`conclusion`) or a StatusContext (`state`).
#[derive(Clone, Debug)]
pub(crate) struct StatusCheck {
    /// The CheckRun's name or the StatusContext's context, trimmed.
    pub(crate) name: Option<String>,
    /// The CheckRun's workflow, trimmed.
    pub(crate) workflow_name: Option<String>,
    /// Branch-protection reading. A completed `SKIPPED` or `NEUTRAL`
    /// conclusion counts as passed. `None` for a value this reader does not
    /// recognize, which can never authorize waiting.
    pub(crate) state: Option<CheckState>,
    /// Owner-owed reading. Only an explicit success is passed: a CheckRun
    /// conclusion `SUCCESS`, or a StatusContext state `SUCCESS`. A completed
    /// `SKIPPED` or `NEUTRAL` conclusion is pending [ORB-15812].
    pub(crate) owed_state: Option<CheckState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckState {
    Pending,
    Passed,
    Failed,
}

impl StatusCheck {
    fn parse(check: &Value) -> Self {
        let state = Self::classify(check);
        Self {
            name: reported(check.get("name").or_else(|| check.get("context"))),
            workflow_name: check
                .get("workflowName")
                .and_then(Value::as_str)
                .map(|workflow| workflow.trim().to_string()),
            state,
            owed_state: Self::classify_owed(check, state),
        }
    }

    /// Branch-protection classification. A completed `SKIPPED` or `NEUTRAL`
    /// conclusion counts as satisfied here. Owner-owed checks use
    /// [`Self::classify_owed`].
    fn classify(check: &Value) -> Option<CheckState> {
        if let Some(state) = check.get("state").and_then(Value::as_str) {
            return match state {
                "PENDING" | "EXPECTED" => Some(CheckState::Pending),
                "SUCCESS" => Some(CheckState::Passed),
                "FAILURE" | "ERROR" => Some(CheckState::Failed),
                _ => None,
            };
        }
        match check.get("status").and_then(Value::as_str)? {
            "QUEUED" | "IN_PROGRESS" | "PENDING" | "REQUESTED" | "WAITING" => {
                Some(CheckState::Pending)
            }
            "COMPLETED" => match check.get("conclusion").and_then(Value::as_str)? {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => Some(CheckState::Passed),
                "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
                | "STALE" => Some(CheckState::Failed),
                _ => None,
            },
            _ => None,
        }
    }

    /// Owner-owed checks accept only an explicit success. A StatusContext
    /// carries `state` and no conclusion, so its `SUCCESS` stays a pass.
    /// A completed CheckRun `SKIPPED` or `NEUTRAL` is not the analysis the
    /// review did not run, so the owner keeps waiting [ORB-15812].
    fn classify_owed(check: &Value, state: Option<CheckState>) -> Option<CheckState> {
        if check.get("state").and_then(Value::as_str).is_some() {
            return state;
        }
        match state {
            Some(CheckState::Passed)
                if check.get("conclusion").and_then(Value::as_str) != Some("SUCCESS") =>
            {
                Some(CheckState::Pending)
            }
            other => other,
        }
    }
}

/// One pull request as `pr.list` reports it.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) struct PullRequestSummary {
    pub(in crate::executor::automation::vcs) number: Option<String>,
    /// The head branch exactly as reported: an empty name is a branch that
    /// does not match, while an absent one is unreadable.
    pub(in crate::executor::automation::vcs) head_branch: Option<String>,
}

impl PullRequestSummary {
    /// Parse a `pr.list` response, which must carry a `pull_requests` array.
    pub(in crate::executor::automation::vcs) fn list_from_response(
        response: &Value,
    ) -> Result<Vec<Self>, OrbitError> {
        let pull_requests = response
            .get("pull_requests")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "private automation VCS PR list did not return pull_requests metadata"
                        .to_string(),
                )
            })?;
        Ok(pull_requests
            .iter()
            .map(|pull_request| Self {
                number: pull_request.get("number").and_then(number_selector),
                head_branch: pull_request
                    .get("headRefName")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
            .collect())
    }
}

/// The repository merge settings and base-branch rule `pr.merge_capabilities`
/// reports for one pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) struct MergeCapabilities {
    /// `owner/name` as the provider reports it, trimmed.
    pub(in crate::executor::automation::vcs) repository: Option<String>,
    pub(in crate::executor::automation::vcs) base_branch: Option<String>,
    pub(in crate::executor::automation::vcs) allow_squash_merge: bool,
    pub(in crate::executor::automation::vcs) allow_rebase_merge: bool,
    pub(in crate::executor::automation::vcs) allow_merge_commit: bool,
    pub(in crate::executor::automation::vcs) requires_linear_history: bool,
    pub(in crate::executor::automation::vcs) allow_auto_merge: bool,
}

impl MergeCapabilities {
    /// Parse a `pr.merge_capabilities` response. Every boolean is required.
    pub(in crate::executor::automation::vcs) fn from_response(
        response: &Value,
    ) -> Result<Self, OrbitError> {
        let repository = response.get("repository").ok_or_else(|| {
            OrbitError::Execution(
                "merge strategy resolution: private VCS response omitted repository capabilities"
                    .to_string(),
            )
        })?;
        let capability = |key: &str| {
            repository.get(key).and_then(Value::as_bool).ok_or_else(|| {
                OrbitError::Execution(format!(
                    "merge strategy resolution: repository capabilities omitted boolean {key}"
                ))
            })
        };
        Ok(Self {
            allow_squash_merge: capability("allow_squash_merge")?,
            allow_rebase_merge: capability("allow_rebase_merge")?,
            allow_merge_commit: capability("allow_merge_commit")?,
            requires_linear_history: capability("requires_linear_history")?,
            allow_auto_merge: capability("allow_auto_merge")?,
            repository: reported(repository.get("name_with_owner")),
            base_branch: repository
                .get("base_branch")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

/// What a `pr.merge` request the provider answered without failing means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) enum ReviewedMergeOutcome {
    /// Accepted. A synchronous reviewed merge names the commit it landed.
    Accepted { landed_commit: Option<String> },
    /// The SHA-conditioned merge was refused because another merge moved the
    /// base first; carries the provider's diagnostic.
    BaseModified { diagnostic: String },
}

impl ReviewedMergeOutcome {
    /// Parse a `pr.merge` response.
    pub(in crate::executor::automation::vcs) fn from_response(response: &Value) -> Self {
        if response.get("refusal").and_then(Value::as_str)
            == Some(operations::BASE_MODIFIED_REFUSAL)
        {
            return Self::BaseModified {
                diagnostic: response
                    .get("stderr")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            };
        }
        Self::Accepted {
            landed_commit: response
                .get("landed_commit")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }
    }
}

/// Read pull request `pr_number`'s merge-relevant state.
pub(in crate::executor::automation::vcs) fn read_pr_status<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    pr_number: &str,
) -> Result<PullRequestStatus, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_STATUS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path.to_string_lossy(),
        }),
    )?;
    Ok(PullRequestStatus::from_status_response(&response))
}

/// The checks branch protection requires on one head commit, as
/// `pr.required_checks` reports them: `(workflow, name)` pairs, matched the
/// way [`StatusCheck`] names a rollup entry.
#[derive(Clone, Debug)]
pub(in crate::executor::automation::vcs) struct RequiredChecks {
    pub(in crate::executor::automation::vcs) head_sha: String,
    pub(in crate::executor::automation::vcs) required: Vec<(Option<String>, String)>,
}

impl RequiredChecks {
    /// Parse a `pr.required_checks` response. A truncated listing, or an
    /// entry without a name or a boolean `isRequired`, is a failed read: it
    /// cannot prove a failed check optional.
    fn from_response(response: &Value) -> Result<Self, OrbitError> {
        let unreadable =
            |what: &str| OrbitError::Execution(format!("the required-checks answer {what}"));
        let pull_request = response
            .get("pull_request")
            .filter(|value| value.is_object())
            .ok_or_else(|| unreadable("carried no pull request"))?;
        let commit = pull_request
            .pointer("/commits/nodes/0/commit")
            .ok_or_else(|| unreadable("carried no head commit"))?;
        let head_sha =
            reported(commit.get("oid")).ok_or_else(|| unreadable("carried no head commit id"))?;
        let Some(contexts) = commit
            .get("statusCheckRollup")
            .filter(|rollup| !rollup.is_null())
            .map(|rollup| &rollup["contexts"])
        else {
            return Ok(Self {
                head_sha,
                required: Vec::new(),
            });
        };
        if contexts.pointer("/pageInfo/hasNextPage") != Some(&Value::Bool(false)) {
            return Err(unreadable("was truncated or carried no page information"));
        }
        let mut required = Vec::new();
        for node in contexts["nodes"]
            .as_array()
            .ok_or_else(|| unreadable("carried no check list"))?
        {
            let name = reported(node.get("name").or_else(|| node.get("context")))
                .ok_or_else(|| unreadable("named no check"))?;
            match node.get("isRequired").and_then(Value::as_bool) {
                Some(true) => required.push((
                    node.pointer("/checkSuite/workflowRun/workflow/name")
                        .and_then(Value::as_str)
                        .map(|workflow| workflow.trim().to_string()),
                    name,
                )),
                Some(false) => {}
                None => {
                    return Err(unreadable(&format!(
                        "did not say whether '{name}' is required"
                    )));
                }
            }
        }
        Ok(Self { head_sha, required })
    }

    /// Whether the rollup entry `(workflow, name)` is required. A rollup
    /// entry without a workflow matches by name alone.
    pub(in crate::executor::automation::vcs) fn requires(
        &self,
        workflow: Option<&str>,
        name: &str,
    ) -> bool {
        self.required
            .iter()
            .any(|(required_workflow, required_name)| {
                required_name == name
                    && (workflow.is_none() || required_workflow.as_deref() == workflow)
            })
    }
}

/// Read which checks branch protection requires on pull request
/// `pr_number`'s head.
pub(in crate::executor::automation::vcs) fn read_required_checks<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    pr_number: &str,
) -> Result<RequiredChecks, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_REQUIRED_CHECKS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path.to_string_lossy(),
        }),
    )?;
    RequiredChecks::from_response(&response)
}

fn reported(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Preserve the engine's accepted PR selector representations without changing
/// the desktop's stricter unsigned-number identity check.
fn number_selector(value: &Value) -> Option<String> {
    value
        .as_i64()
        .map(|number| number.to_string())
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_str().map(ToOwned::to_owned))
}
