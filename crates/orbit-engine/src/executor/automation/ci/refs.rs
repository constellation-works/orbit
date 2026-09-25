//! Which heads a CI evidence snapshot scans, and which red-run branches are
//! retired or unverified on origin.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::collect::{Bounds, push_retryable_error};
use super::optional_input_string;
use super::partition::{run_branch, run_is_completed, run_is_unsuccessful};
use super::query::{CiQueries, RemoteBranchHeads};

/// Which of the workspace's heads a run belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RefKind {
    Integration,
    Release,
    PullRequest,
}

impl RefKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Integration => "integration",
            Self::Release => "release",
            Self::PullRequest => "pull_request",
        }
    }
}

/// One head to scan, with the SHA it currently points at.
pub(super) struct ScannedRef {
    pub(super) kind: RefKind,
    pub(super) branch: String,
    pub(super) head_sha: Option<String>,
    pub(super) pr_number: Option<Value>,
    pub(super) pr_url: Option<Value>,
}

/// Work out which heads to scan.
///
/// The integration branch comes from the run's own base branch — the branch
/// this workspace actually ships onto — and the release branch from what
/// GitHub reports as the repository default. Neither is guessed from a naming
/// convention, and when the two coincide the ref is scanned once.
pub(super) fn derive_refs<Q: CiQueries + ?Sized>(
    queries: &Q,
    branch_heads: &RemoteBranchHeads,
    input: &Value,
    default_branch: Option<&str>,
    bounds: &Bounds,
    notes: &mut Vec<String>,
    retryable_errors: &mut Vec<Value>,
) -> Result<Vec<ScannedRef>, OrbitError> {
    let integration = optional_input_string(input, "integration_branch")
        .or_else(|| optional_input_string(input, "base_branch"))
        .or_else(|| default_branch.map(ToOwned::to_owned));
    let mut refs: Vec<ScannedRef> = Vec::new();

    for (kind, branch) in [
        (RefKind::Integration, integration),
        (RefKind::Release, default_branch.map(ToOwned::to_owned)),
    ] {
        let Some(branch) = branch else {
            notes.push(format!(
                "no {} branch could be derived; that head was not scanned",
                kind.as_str()
            ));
            continue;
        };
        if refs.iter().any(|scanned| scanned.branch == branch) {
            continue;
        }
        let head_sha = match branch_heads.head(&branch) {
            Ok(head) => head,
            Err(error) => {
                push_retryable_error(
                    retryable_errors,
                    "discovery",
                    "remote_branch_head",
                    None,
                    &format!("branch {branch}: {error}"),
                );
                None
            }
        };
        if head_sha.is_none() {
            notes.push(format!(
                "{} branch '{branch}' has no head on origin; failures there cannot be \
                 compared against a current head",
                kind.as_str()
            ));
        }
        refs.push(ScannedRef {
            kind,
            branch,
            head_sha,
            pr_number: None,
            pr_url: None,
        });
    }

    match queries.open_pull_requests(bounds.max_pull_requests) {
        Ok(pull_requests) => {
            if pull_requests.len() as u64 == bounds.max_pull_requests {
                notes.push(format!(
                    "open pull requests were listed at the cap ({}); further open \
                     pull-request heads may exist and were not scanned",
                    bounds.max_pull_requests
                ));
            }
            for pull_request in pull_requests {
                let Some(branch) = pull_request
                    .get("head_branch")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                // A head already scanned as the integration or release branch
                // would otherwise be listed twice, reporting each of its
                // failures twice.
                if refs.iter().any(|scanned| scanned.branch == branch) {
                    continue;
                }
                refs.push(ScannedRef {
                    kind: RefKind::PullRequest,
                    branch: branch.to_string(),
                    head_sha: pull_request
                        .get("reported_head_sha")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    pr_number: pull_request.get("number").cloned(),
                    pr_url: pull_request.get("url").cloned(),
                });
            }
        }
        Err(error) => {
            push_retryable_error(
                retryable_errors,
                "discovery",
                "pr_list",
                None,
                &error.to_string(),
            );
            notes.push(
                "open pull requests could not be listed; no pull-request head was scanned"
                    .to_string(),
            );
        }
    }

    Ok(refs)
}

pub(super) struct CandidateProbeResults {
    pub(super) retired: std::collections::BTreeSet<String>,
    pub(super) unverified: std::collections::BTreeMap<String, (String, String)>,
}

/// Branches that carry a red run but no longer exist on origin, or whose
/// current relevance could not be verified within probe bounds.
///
/// A task branch is deleted when its pull request merges, so its old red runs
/// describe code that either landed — where the landing branch's own runs are
/// the current evidence — or was abandoned. Either way there is no ref left to
/// fix, and treating those runs as current is what let a backlog of merged
/// historical pull requests consume every sweep's investigation budget.
///
/// The probe is authoritative (origin, not a naming convention) and bounded:
/// one repository-wide head query supplies local answers for every branch that
/// actually carries a red run, and none of those answers are requested for a
/// branch already scanned as a landing head or an open pull request. A probe
/// that fails or is skipped due to probe budget keeps its branch deferred rather
/// than assuming it is merged or current.
pub(super) fn probe_branches(
    branch_heads: &RemoteBranchHeads,
    refs: &[ScannedRef],
    runs: &[Value],
    bounds: &Bounds,
    notes: &mut Vec<String>,
) -> CandidateProbeResults {
    let mut candidates: Vec<&str> = Vec::new();
    for run in runs {
        let branch = run_branch(run);
        if branch.is_empty() || !run_is_completed(run) || !run_is_unsuccessful(run) {
            continue;
        }
        if refs.iter().any(|scanned| scanned.branch == branch) || candidates.contains(&branch) {
            continue;
        }
        candidates.push(branch);
    }

    let selected = probe_slots(
        candidates.len(),
        bounds.max_retired_ref_probes,
        bounds.investigation_cursor,
    );

    if candidates.len() > selected.len() {
        notes.push(format!(
            "{} branch(es) carrying red runs were not probed against origin \
             (max_retired_ref_probes={}); their failures remain deferred until verified",
            candidates.len() - selected.len(),
            bounds.max_retired_ref_probes
        ));
    }

    let mut retired = std::collections::BTreeSet::new();
    let mut unverified = std::collections::BTreeMap::new();

    for (index, branch) in candidates.iter().enumerate() {
        if !selected.contains(&index) {
            unverified.insert(
                (*branch).to_string(),
                (
                    "retired_ref_budget".to_string(),
                    format!(
                        "candidate branch '{branch}' was not probed against origin because \
                         max_retired_ref_probes ({}) was exhausted; its failure remains deferred \
                         until verified",
                        bounds.max_retired_ref_probes
                    ),
                ),
            );
            continue;
        }

        match branch_heads.head(branch) {
            Ok(None) => {
                retired.insert((*branch).to_string());
            }
            Ok(Some(_)) => {}
            Err(error) => {
                notes.push(format!(
                    "branch '{branch}' could not be checked against origin ({error}); its \
                     failure remains deferred until verified"
                ));
                unverified.insert(
                    (*branch).to_string(),
                    (
                        "remote_branch_head".to_string(),
                        format!(
                            "candidate branch '{branch}' could not be checked against origin \
                             ({error}); its failure remains deferred until verified"
                        ),
                    ),
                );
            }
        }
    }

    CandidateProbeResults {
        retired,
        unverified,
    }
}

/// Which candidate branches this sweep probes against origin.
///
/// If candidate count exceeds the probe budget, the prefix of slots probes the
/// newest candidates while the final slot rotates through overflow candidates
/// with each advancing cursor so all candidates eventually get probed without
/// starvation.
fn probe_slots(candidates: usize, budget: usize, cursor: u64) -> std::collections::BTreeSet<usize> {
    let attempted = candidates.min(budget);
    let mut slots: std::collections::BTreeSet<usize> = (0..attempted).collect();
    if candidates <= attempted || attempted == 0 {
        return slots;
    }
    let rotating = attempted - 1;
    slots.remove(&rotating);
    let overflow = candidates - rotating;
    slots.insert(rotating + (cursor % overflow as u64) as usize);
    slots
}

pub(super) fn head_json(scanned: &ScannedRef) -> Value {
    json!({
        "kind": scanned.kind.as_str(),
        "branch": scanned.branch,
        "current_head_sha": scanned.head_sha,
        "pr_number": scanned.pr_number,
        "pr_url": scanned.pr_url,
    })
}
