//! The forge a task's terminal decision closes Orbit-authored PRs through.
//!
//! When a task lands, is rejected or is archived, Core closes the delivery
//! and `[BLOCKED]` PRs Orbit opened for it (`application::task::pr_closure`),
//! and an owner drain checks a dead delivery leaf's PR is still open before
//! it adopts the leaf. When an operator merges a task's delivery PR outside
//! Orbit, Core settles the task from that merge
//! (`application::task::external_merge`). The blocked-task triage re-requests
//! the failed checks on a blocked task's PR head. Those policies only need a few
//! forge operations, so they reach them through [`TaskPrForge`] rather than
//! the `gh` CLI directly: production uses [`GhTaskPrForge`], and fixtures
//! inject a fake forge. A resident holding the merge right lands a task's
//! delivery PR through it (`application::task::external_merge`). No operation
//! deletes a branch.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_exec::{ExecRequest, NoSandbox, run_process};
use serde_json::Value;

/// One open pull request as the closure policy matches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePullRequest {
    /// Provider PR number.
    pub number: u64,
    /// Head branch name.
    pub head_branch: String,
    /// PR description.
    pub body: String,
    /// Login of the PR's author.
    pub author: String,
}

/// One merged pull request as external-merge settlement matches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedPullRequest {
    /// The fields an open pull request is matched by.
    pub pull: ForgePullRequest,
    /// The branch it merged into.
    pub base_branch: String,
    /// The PR head observed after merge, including an operator's rebase.
    pub head_sha: String,
    /// The commit it merged as.
    pub merge_commit: String,
}

/// State of the PR a lease's branch or explicit `#number` binds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasePullRequest {
    pub number: u64,
    pub base_branch: String,
    pub merged: bool,
}

/// What holds an open pull request back from merging, as an out-of-run
/// auto-fix repair reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestBlockers {
    /// The fields an Orbit pull request is matched by.
    pub pull: ForgePullRequest,
    /// The head commit the answer describes.
    pub head_sha: String,
    /// The branch conflicts with its base.
    pub conflicting: bool,
    /// Required checks on the head that failed, by name.
    pub failed_required_checks: Vec<String>,
    /// Required checks on the head that have not finished, by name.
    pub pending_required_checks: Vec<String>,
    /// Required checks on the head that passed, skipped or ended neutral, by
    /// name. With the other two lists empty, an empty list means the head
    /// reports no required check at all.
    pub passed_required_checks: Vec<String>,
}

/// Open pull request state needed for an operator's failure-handoff landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffLandingPullRequest {
    /// Exact head and required checks, with known mergeability.
    pub blockers: PullRequestBlockers,
    /// Destination branch.
    pub base_branch: String,
    /// Whether the author still marks the pull request as a draft.
    pub draft: bool,
}

/// Outcomes of requests made to rerun failed workflow jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRerunOutcome {
    /// Successfully acknowledged workflow requests, including their run IDs.
    pub requested: Vec<(u64, String)>,
    /// The first request whose outcome could not be confirmed, and its error.
    pub uncertain: Option<(u64, String, String)>,
}

/// How a request to merge a pull request ended when the forge answered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullRequestMergeOutcome {
    /// The forge merged it synchronously.
    Merged {
        /// The commit it merged as.
        merge_commit: String,
    },
    /// The forge refused it, so nothing merged: branch protection, a moved
    /// head, a method the repository forbids or a caller it does not permit.
    Refused {
        /// The forge's own reason.
        reason: String,
    },
}

/// Forge operations behind closing a task's PRs on a terminal decision,
/// settling a task from its merged delivery PR, landing one, and finding the
/// merge blockers an out-of-run auto-fix repairs.
pub trait TaskPrForge: Send + Sync {
    /// The PR for this binding, including an unmerged PR. `None` proves the
    /// branch has no PR; errors must never fall back to ancestry. The caller
    /// checks the returned base, rather than treating a different base as no PR.
    fn lease_pull_request(
        &self,
        _repo_root: &Path,
        _binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        Err(OrbitError::Execution("lease PR lookup unavailable".into()))
    }

    /// Every open pull request of the repository checked out at `repo_root`.
    fn open_pull_requests(&self, repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError>;

    /// Pull request `number` when it is open, read on its own so a
    /// repository with more open pull requests than one listing returns
    /// still answers. The default reads it from that listing.
    fn open_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<ForgePullRequest>, OrbitError> {
        Ok(self
            .open_pull_requests(repo_root)?
            .into_iter()
            .find(|pull| pull.number == number))
    }

    /// The repository's most recently merged pull requests. The default
    /// reports none: a forge that cannot read merges settles nothing.
    fn merged_pull_requests(
        &self,
        _repo_root: &Path,
    ) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    /// Pull request `number` when it is merged, read on its own so a delivery
    /// merged before the latest listing still answers. The default reads it
    /// from that listing.
    fn merged_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        Ok(self
            .merged_pull_requests(repo_root)?
            .into_iter()
            .find(|merged| merged.pull.number == number))
    }

    /// What holds pull request `number` back from merging, or `None` when it
    /// is not open or this forge cannot tell. GitHub's still-calculating
    /// `UNKNOWN` mergeability answer is reported as
    /// [`OrbitError::PullRequestMergeabilityUnknown`]. The default cannot tell,
    /// so a forge without it repairs nothing out of run. A caller that needs
    /// the required checks while mergeability is still calculating uses
    /// [`Self::pull_request_required_checks`].
    fn pull_request_blockers(
        &self,
        _repo_root: &Path,
        _number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        Ok(None)
    }

    /// Open state for an owner landing. Unsupported forges fail closed.
    fn handoff_landing_pull_request(
        &self,
        _repo_root: &Path,
        _number: u64,
    ) -> Result<Option<HandoffLandingPullRequest>, OrbitError> {
        Err(OrbitError::Execution(
            "handoff landing PR lookup unavailable".into(),
        ))
    }

    /// Required checks on open pull request `number`, without treating unknown
    /// mergeability as unread. `None` when the pull request is not open.
    /// `conflicting` is true only when the forge reports a conflict. The
    /// default asks [`Self::pull_request_blockers`], so a forge that has not
    /// separated the two still refuses while mergeability is unknown.
    fn pull_request_required_checks(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        self.pull_request_blockers(repo_root, number)
    }

    /// Re-request the checks that failed on exactly commit `head_sha` of pull
    /// request `number`, returning acknowledged requests and any uncertain
    /// request. Errors must occur before a request is attempted; once one is
    /// attempted, return its outcome even if the transport fails. Nothing is
    /// re-requested for any other head. The default refuses before a request.
    fn rerun_failed_checks(
        &self,
        _repo_root: &Path,
        number: u64,
        _head_sha: &str,
    ) -> Result<CheckRerunOutcome, OrbitError> {
        Err(OrbitError::Execution(format!(
            "this forge cannot re-request the checks of pull request #{number}"
        )))
    }

    /// Merge pull request `number` at exactly commit `head_sha`, under every
    /// rule the forge enforces. An error means the outcome is unknown or no
    /// request was made; a refusal the forge answered is
    /// [`PullRequestMergeOutcome::Refused`]. The default refuses before a
    /// request.
    fn merge_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        _head_sha: &str,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        Err(OrbitError::Execution(format!(
            "this forge cannot merge pull request #{number}"
        )))
    }

    /// The login this machine's forge credentials act as, which is the
    /// identity Orbit opens its PRs under.
    fn authenticated_login(&self, repo_root: &Path) -> Result<String, OrbitError>;

    /// Close one pull request with `comment`, keeping its head branch.
    fn close_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError>;
}

/// The forge for a host-bound runtime. This crate's own unit tests never
/// reach a real forge: a done or rejected fixture task must not touch a
/// repository's PRs from whichever checkout the tests run in.
#[must_use]
pub fn default_task_pr_forge() -> Arc<dyn TaskPrForge> {
    if cfg!(test) {
        Arc::new(NoTaskPrForge)
    } else {
        Arc::new(GhTaskPrForge)
    }
}

/// A forge with no PRs, for runtimes that are not bound to a repository.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTaskPrForge;

impl TaskPrForge for NoTaskPrForge {
    fn lease_pull_request(
        &self,
        _repo_root: &Path,
        _binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        Ok(None)
    }

    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Err(OrbitError::Execution(
            "no forge is bound to this runtime".to_string(),
        ))
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        _comment: &str,
    ) -> Result<(), OrbitError> {
        Err(OrbitError::Execution(format!(
            "no forge is bound to this runtime to close pull request #{number}"
        )))
    }
}

/// GitHub through the `gh` CLI, run in the workspace checkout so `gh`
/// resolves the repository from its remotes. The argv lives in
/// `orbit_tools::github_cli`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhTaskPrForge;

impl TaskPrForge for GhTaskPrForge {
    fn lease_pull_request(
        &self,
        repo_root: &Path,
        binding: &str,
    ) -> Result<Option<LeasePullRequest>, OrbitError> {
        let entry = if let Some(number) = binding.strip_prefix('#') {
            let number = number.parse::<u64>().map_err(|_| {
                OrbitError::InvalidInput("lease PR binding must be #<number>".into())
            })?;
            view_pull_request(repo_root, number)?
        } else {
            let stdout = task_forge_gh_request(
                orbit_tools::github_cli::bound_branch_pull_request_request(binding),
                repo_root,
                "gh pr list",
            )?;
            let entries = orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr list")?;
            let entries = entries.as_array().ok_or_else(|| {
                OrbitError::Execution("gh pr list did not return an array".into())
            })?;
            let Some(entry) = entries.first() else {
                return Ok(None);
            };
            entry.clone()
        };
        let state = entry
            .get("state")
            .and_then(Value::as_str)
            .filter(|state| {
                ["open", "closed", "merged"]
                    .iter()
                    .any(|known| state.eq_ignore_ascii_case(known))
            })
            .ok_or_else(|| {
                OrbitError::Execution("lease PR lookup returned no valid state".into())
            })?;
        Ok(Some(LeasePullRequest {
            number: entry.get("number").and_then(Value::as_u64).ok_or_else(|| {
                OrbitError::Execution("lease PR lookup returned no number".into())
            })?,
            base_branch: entry
                .get("baseRefName")
                .and_then(Value::as_str)
                .ok_or_else(|| OrbitError::Execution("lease PR lookup returned no base".into()))?
                .into(),
            merged: state.eq_ignore_ascii_case("merged"),
        }))
    }

    fn open_pull_requests(&self, repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let stdout = task_forge_gh_request(
            orbit_tools::github_cli::open_pull_requests_request(),
            repo_root,
            "gh pr list",
        )?;
        let entries = orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr list")?;
        let entries = entries.as_array().ok_or_else(|| {
            OrbitError::Execution("gh pr list did not return an array".to_string())
        })?;
        Ok(entries.iter().filter_map(pull_request_from_json).collect())
    }

    fn open_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<ForgePullRequest>, OrbitError> {
        let entry = view_pull_request(repo_root, number)?;
        if !has_state(&entry, "open") {
            return Ok(None);
        }
        pull_request_from_json(&entry).map(Some).ok_or_else(|| {
            OrbitError::Execution(format!(
                "gh pr view #{number} did not report its number, head branch and author"
            ))
        })
    }

    fn merged_pull_requests(&self, repo_root: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        let stdout = task_forge_gh_request(
            orbit_tools::github_cli::merged_pull_requests_request(),
            repo_root,
            "gh pr list",
        )?;
        let entries = orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr list")?;
        let entries = entries.as_array().ok_or_else(|| {
            OrbitError::Execution("gh pr list did not return an array".to_string())
        })?;
        Ok(entries
            .iter()
            .filter_map(merged_pull_request_from_json)
            .collect())
    }

    fn merged_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        let entry = view_pull_request(repo_root, number)?;
        if !has_state(&entry, "merged") {
            return Ok(None);
        }
        merged_pull_request_from_json(&entry)
            .map(Some)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "gh pr view #{number} did not report its head branch, head commit, author, base branch \
                     and merge commit"
                ))
            })
    }

    fn pull_request_blockers(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        let Some(pull) = open_pull_request_document(repo_root, number)? else {
            return Ok(None);
        };
        // Unknown mergeability is its own answer: callers that merge or defer
        // must not treat it as a conflict, a clean merge, or an unreadable
        // rollup. Check reads use `pull_request_required_checks` instead.
        if pull.get("mergeable").and_then(Value::as_str) == Some("UNKNOWN") {
            return Err(OrbitError::PullRequestMergeabilityUnknown { number });
        }
        if !matches!(
            pull.get("mergeable").and_then(Value::as_str),
            Some("MERGEABLE" | "CONFLICTING")
        ) {
            return Err(unreadable_pull_request_blockers(number));
        }
        pull_request_blockers_from_json(number, &pull)
            .map(Some)
            .ok_or_else(|| unreadable_pull_request_blockers(number))
    }

    fn handoff_landing_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<HandoffLandingPullRequest>, OrbitError> {
        let Some(pull) = open_pull_request_document(repo_root, number)? else {
            return Ok(None);
        };
        if pull.get("mergeable").and_then(Value::as_str) == Some("UNKNOWN") {
            return Err(OrbitError::PullRequestMergeabilityUnknown { number });
        }
        if !matches!(
            pull.get("mergeable").and_then(Value::as_str),
            Some("MERGEABLE" | "CONFLICTING")
        ) {
            return Err(unreadable_pull_request_blockers(number));
        }
        let blockers = pull_request_blockers_from_json(number, &pull)
            .ok_or_else(|| unreadable_pull_request_blockers(number))?;
        let base_branch = pull
            .get("baseRefName")
            .and_then(Value::as_str)
            .filter(|base| !base.is_empty())
            .ok_or_else(|| unreadable_pull_request_blockers(number))?
            .to_string();
        let draft = pull
            .get("isDraft")
            .and_then(Value::as_bool)
            .ok_or_else(|| unreadable_pull_request_blockers(number))?;
        Ok(Some(HandoffLandingPullRequest {
            blockers,
            base_branch,
            draft,
        }))
    }

    fn pull_request_required_checks(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<PullRequestBlockers>, OrbitError> {
        let Some(pull) = open_pull_request_document(repo_root, number)? else {
            return Ok(None);
        };
        pull_request_blockers_from_json(number, &pull)
            .map(Some)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "gh api graphql did not report the author, head and complete required \
                     checks of pull request #{number}"
                ))
            })
    }

    /// Waits up to 120 seconds for in-progress workflows on the head, then
    /// reruns failed jobs. A run on another head is never touched.
    fn rerun_failed_checks(
        &self,
        repo_root: &Path,
        number: u64,
        head_sha: &str,
    ) -> Result<CheckRerunOutcome, OrbitError> {
        super::pr_forge_rerun::rerun_failed_head_workflows(repo_root, number, head_sha)
    }

    fn merge_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
        head_sha: &str,
    ) -> Result<PullRequestMergeOutcome, OrbitError> {
        if !matches!(head_sha.len(), 40 | 64) || !head_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OrbitError::InvalidInput(format!(
                "merging pull request #{number} requires its exact head commit"
            )));
        }
        let stdout = task_forge_gh_request(
            orbit_tools::github_cli::repository_merge_methods_request(),
            repo_root,
            "gh repo view",
        )?;
        let methods = orbit_tools::github_cli::parse_gh_json(&stdout, "gh repo view")?;
        let Some(method) = [
            ("squashMergeAllowed", "squash"),
            ("rebaseMergeAllowed", "rebase"),
            ("mergeCommitAllowed", "merge"),
        ]
        .into_iter()
        .find_map(|(field, method)| {
            (methods.get(field).and_then(Value::as_bool) == Some(true)).then_some(method)
        }) else {
            return Ok(PullRequestMergeOutcome::Refused {
                reason: "the repository permits no merge method".to_string(),
            });
        };
        let mut request =
            orbit_tools::github_cli::merge_pull_request_at_head_request(number, head_sha, method);
        request.current_dir = Some(repo_root.to_string_lossy().into_owned());
        let result = run_process(&request, &NoSandbox)?;
        if !result.success {
            // This text is returned to an agent's call.
            let stderr =
                orbit_common::security::redaction::redact_all_and_home(result.stderr.trim());
            if !result.timed_out && forge_refused_request(&stderr) {
                return Ok(PullRequestMergeOutcome::Refused { reason: stderr });
            }
            return Err(OrbitError::Execution(format!(
                "gh api merge of pull request #{number} failed: {stderr}"
            )));
        }
        let answer = orbit_tools::github_cli::parse_gh_json(&result.stdout, "gh api merge")?;
        match answer.get("sha").and_then(Value::as_str) {
            Some(merge_commit) if answer.get("merged").and_then(Value::as_bool) == Some(true) => {
                Ok(PullRequestMergeOutcome::Merged {
                    merge_commit: merge_commit.to_string(),
                })
            }
            _ => Err(OrbitError::Execution(format!(
                "gh api merge of pull request #{number} did not confirm a merge"
            ))),
        }
    }

    fn authenticated_login(&self, repo_root: &Path) -> Result<String, OrbitError> {
        let stdout = task_forge_gh_request(
            orbit_tools::github_cli::authenticated_login_request(),
            repo_root,
            "gh api user",
        )?;
        let login = stdout.trim();
        if login.is_empty() {
            return Err(OrbitError::Execution(
                "gh api user returned no login".to_string(),
            ));
        }
        Ok(login.to_string())
    }

    fn close_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError> {
        task_forge_gh_request(
            orbit_tools::github_cli::close_pull_request_request(number, comment),
            repo_root,
            "gh pr close",
        )
        .map(|_| ())
    }
}

fn view_pull_request(repo_root: &Path, number: u64) -> Result<Value, OrbitError> {
    let stdout = task_forge_gh_request(
        orbit_tools::github_cli::view_pull_request_request(number),
        repo_root,
        "gh pr view",
    )?;
    orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr view")
}

/// Whether a failed `gh api` call's stderr is the forge's own rejection of the
/// request: an HTTP 4xx, which `gh` reports as `gh: <reason> (HTTP 405)`. A
/// timed-out request or rate limit, a server error and a transport failure
/// leave the outcome unknown or say nothing about the request.
fn forge_refused_request(stderr: &str) -> bool {
    stderr
        .rsplit_once("(HTTP ")
        .and_then(|(_, status)| status.split_once(')'))
        .and_then(|(status, _)| status.parse::<u16>().ok())
        .is_some_and(|status| (400..500).contains(&status) && !matches!(status, 408 | 429))
}

fn has_state(entry: &Value, state: &str) -> bool {
    entry
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|reported| reported.eq_ignore_ascii_case(state))
}

pub(super) fn task_forge_gh_request(
    mut request: ExecRequest,
    repo_root: &Path,
    label: &str,
) -> Result<String, OrbitError> {
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    let result = run_process(&request, &NoSandbox)?;
    if !result.success {
        return Err(OrbitError::Execution(format!(
            "{label} failed: {}",
            result.stderr.trim()
        )));
    }
    Ok(result.stdout)
}

/// An entry missing a number, head branch or author cannot be matched safely,
/// so it is skipped rather than guessed at.
fn pull_request_from_json(entry: &Value) -> Option<ForgePullRequest> {
    Some(ForgePullRequest {
        number: entry.get("number")?.as_u64()?,
        head_branch: entry.get("headRefName")?.as_str()?.to_string(),
        body: entry
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        author: entry.pointer("/author/login")?.as_str()?.to_string(),
    })
}

/// A merged entry also needs the branch it merged into and its merge commit;
/// without them it cannot prove where the task's work landed.
fn merged_pull_request_from_json(entry: &Value) -> Option<MergedPullRequest> {
    let non_empty = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(MergedPullRequest {
        pull: pull_request_from_json(entry)?,
        base_branch: non_empty(entry.get("baseRefName"))?,
        head_sha: non_empty(entry.get("headRefOid"))?,
        merge_commit: non_empty(entry.pointer("/mergeCommit/oid"))?,
    })
}

fn open_pull_request_document(repo_root: &Path, number: u64) -> Result<Option<Value>, OrbitError> {
    let stdout = task_forge_gh_request(
        orbit_tools::github_cli::pull_request_blockers_request(number),
        repo_root,
        "gh api graphql",
    )?;
    let answer = orbit_tools::github_cli::parse_gh_json(&stdout, "gh api graphql")?;
    let Some(pull) = answer
        .pointer("/data/repository/pullRequest")
        .filter(|pull| !pull.is_null())
        .cloned()
    else {
        return Err(OrbitError::Execution(format!(
            "gh api graphql found no pull request #{number}"
        )));
    };
    if !has_state(&pull, "open") {
        return Ok(None);
    }
    Ok(Some(pull))
}

fn unreadable_pull_request_blockers(number: u64) -> OrbitError {
    OrbitError::Execution(format!(
        "gh api graphql did not report the author, head, mergeability and complete \
         required checks of pull request #{number}"
    ))
}

/// Read a GraphQL pull request answer's required checks. `conflicting` is
/// true only for `CONFLICTING`; `UNKNOWN` and any other spelling are not a
/// conflict, and a missing mergeability field does not hide the checks. A
/// head the rollup does not describe, or a truncated rollup, cannot be
/// judged, so neither is reported as no blocker. Every context must say
/// whether it is required: an unreadable flag cannot prove a failed or
/// unfinished check optional.
fn pull_request_blockers_from_json(number: u64, pull: &Value) -> Option<PullRequestBlockers> {
    let head_sha = pull.get("headRefOid")?.as_str()?.to_string();
    let conflicting = pull.get("mergeable").and_then(Value::as_str) == Some("CONFLICTING");
    let commit = pull.pointer("/commits/nodes/0/commit")?;
    if commit.get("oid")?.as_str()? != head_sha {
        return None;
    }
    let mut failed_required_checks = Vec::new();
    let mut pending_required_checks = Vec::new();
    let mut passed_required_checks = Vec::new();
    if let Some(contexts) = commit
        .get("statusCheckRollup")
        .filter(|rollup| !rollup.is_null())
        .map(|rollup| rollup.get("contexts"))
    {
        let contexts = contexts?;
        if contexts.pointer("/pageInfo/hasNextPage")?.as_bool()? {
            return None;
        }
        for context in contexts.get("nodes")?.as_array()? {
            if !context.get("isRequired")?.as_bool()? {
                continue;
            }
            let name = context
                .get("name")
                .or_else(|| context.get("context"))
                .and_then(Value::as_str)
                .unwrap_or("unnamed check")
                .to_string();
            if required_context_failed(context) {
                failed_required_checks.push(name);
            } else if required_context_passed(context) {
                passed_required_checks.push(name);
            } else {
                pending_required_checks.push(name);
            }
        }
    }
    Some(PullRequestBlockers {
        pull: ForgePullRequest {
            number,
            head_branch: pull.get("headRefName")?.as_str()?.to_string(),
            body: pull
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            author: pull.pointer("/author/login")?.as_str()?.to_string(),
        },
        head_sha,
        conflicting,
        failed_required_checks,
        pending_required_checks,
        passed_required_checks,
    })
}

/// Whether a required status context failed, as completion's watcher
/// classifies it: a status `FAILURE` or `ERROR`, or a completed check run
/// that did not succeed, skip or end neutral.
fn required_context_failed(context: &Value) -> bool {
    if let Some(state) = context.get("state").and_then(Value::as_str) {
        return matches!(state, "FAILURE" | "ERROR");
    }
    context.get("status").and_then(Value::as_str) == Some("COMPLETED")
        && matches!(
            context.get("conclusion").and_then(Value::as_str),
            Some(
                "FAILURE"
                    | "CANCELLED"
                    | "TIMED_OUT"
                    | "ACTION_REQUIRED"
                    | "STARTUP_FAILURE"
                    | "STALE"
            )
        )
}

/// Whether a required status context passed: a status `SUCCESS`, or a
/// completed check run that succeeded, skipped or ended neutral. A context
/// neither failed nor passed has not finished.
fn required_context_passed(context: &Value) -> bool {
    if let Some(state) = context.get("state").and_then(Value::as_str) {
        return state == "SUCCESS";
    }
    context.get("status").and_then(Value::as_str) == Some("COMPLETED")
        && matches!(
            context.get("conclusion").and_then(Value::as_str),
            Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
        )
}
