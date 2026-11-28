//! Read-only observation of owner-base setup refusals.

use super::source::{
    AttentionCandidates, AttentionSourceContext, attention_reason, workspace_degraded,
};
use orbit_common::OrbitError;
use orbit_common::governance::attention::cursor;
use orbit_store::contract::JobRunQuery;
use orbit_types::tool::{AttentionItem, AttentionKind, AttentionNext, AttentionSeverity};
use orbit_types::workflow::{DELIVERY_LEAF_JOBS, JobRun, JobRunState};
use serde_json::json;
use std::time::Instant;

const KIND: AttentionKind = AttentionKind::DeliveryStaleBase;

// The setup error is the durable evidence of the precise branch and checkout.
// Retained candidate-branch refusals have a different message and are excluded.
fn refusal(message: &str) -> Option<(&str, Option<&str>)> {
    let rest = message
        .split_once("worktree setup refusing stale base: ")?
        .1;
    if let Some(rest) = rest.strip_prefix("owner checkout '") {
        let (checkout, rest) = rest.split_once("' holds '")?;
        let (branch, _) = rest.split_once("' at ")?;
        Some((branch, Some(checkout)))
    } else {
        let rest = rest.strip_prefix("no checkout holds '")?;
        Some((rest.split_once("', which is at ")?.0, None))
    }
}

fn task_from_message(message: &str) -> Option<&str> {
    message
        .split('[')
        .skip(1)
        .filter_map(|part| part.split_once(']').map(|v| v.0))
        .find(|id| {
            id.rsplit_once('-').is_some_and(|(prefix, number)| {
                !prefix.is_empty()
                    && prefix
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
                    && !number.is_empty()
                    && number.bytes().all(|b| b.is_ascii_digit())
            })
        })
}

fn hold(run: &JobRun) -> Option<(&str, Option<&str>)> {
    let step = run
        .steps
        .iter()
        .find(|step| step.target_id == "worktree_setup")?;
    refusal(step.error_message.as_deref()?)
}

fn observe(
    context: &AttentionSourceContext<'_>,
    run: &JobRun,
    branch: &str,
    checkout: Option<&str>,
) -> Result<Option<AttentionItem>, OrbitError> {
    let repo = git2::Repository::open(&context.runtime.paths().repo_root)
        .map_err(|e| OrbitError::Execution(e.to_string()))?;
    let resolve = |name: &str| {
        repo.find_reference(name)
            .and_then(|r| r.peel_to_commit())
            .map(|c| c.id())
    };
    let (owner, remote) = match (
        resolve(&format!("refs/heads/{branch}")),
        resolve(&format!("refs/remotes/origin/{branch}")),
    ) {
        (Ok(owner), Ok(remote)) => (owner, remote),
        _ => {
            return Err(OrbitError::Execution(format!(
                "Cannot resolve local or origin ref for {branch}"
            )));
        }
    };
    let (ahead, behind) = repo
        .graph_ahead_behind(owner, remote)
        .map_err(|e| OrbitError::Execution(e.to_string()))?;
    let dirty = if ahead == 0 && behind > 0 {
        if let Some(path) = checkout {
            let holder =
                git2::Repository::open(path).map_err(|e| OrbitError::Execution(e.to_string()))?;
            let attached = holder
                .head()
                .map_err(|e| OrbitError::Execution(e.to_string()))?
                .shorthand()
                .ok()
                .map(str::to_owned);
            let mut options = git2::StatusOptions::new();
            options.include_untracked(false);
            attached.as_deref() == Some(branch)
                && !holder
                    .statuses(Some(&mut options))
                    .map_err(|e| OrbitError::Execution(e.to_string()))?
                    .is_empty()
        } else {
            false
        }
    } else {
        false
    };
    if behind == 0 || (ahead == 0 && !dirty) {
        return Ok(None);
    }
    let mut walk = repo
        .revwalk()
        .map_err(|e| OrbitError::Execution(e.to_string()))?;
    walk.push(owner)
        .and_then(|()| walk.hide(remote))
        .map_err(|e| OrbitError::Execution(e.to_string()))?;
    let mut commits = Vec::new();
    for oid in walk {
        let commit = repo
            .find_commit(oid.map_err(|e| OrbitError::Execution(e.to_string()))?)
            .map_err(|e| OrbitError::Execution(e.to_string()))?;
        let message = commit.summary().ok().flatten().unwrap_or("");
        commits.push(json!({
            "sha": commit.id().to_string(),
            "summary": message,
            "task_id": task_from_message(message),
        }));
    }
    let commit_hint = commits
        .first()
        .and_then(|c| c["sha"].as_str())
        .unwrap_or("none")
        .to_owned();
    let task_hint = commits
        .first()
        .and_then(|c| c["task_id"].as_str())
        .map(|id| format!(" ({id})"))
        .unwrap_or_default();
    let detail = json!({
        "base": branch,
        "ahead": ahead,
        "behind": behind,
        "owner_commit": owner.to_string(),
        "origin_commit": remote.to_string(),
        "owner_only_commits": commits,
        "tracked_changes": dirty,
    });
    let refs = json!({});
    let since = run.finished_at.unwrap_or(run.created_at);
    let mut item = AttentionItem {
        id: cursor::attention_item_id(KIND, &context.host.machine_id, &context.workspace.id, &refs),
        kind: KIND,
        severity: AttentionSeverity::Medium,
        workspace: context.workspace.clone(),
        host: context.host.clone(),
        refs,
        task: None,
        since: Some(since),
        age_seconds: Some((context.now - since).num_seconds().max(0) as u64),
        expires_at: None,
        reason: attention_reason(&format!(
            "Deliveries held: {branch} is ahead {ahead}, behind {behind} of origin/{branch}; owner-only commit {commit_hint}{task_hint}"
        )),
        detail: detail.clone(),
        next: AttentionNext {
            summary: concat!(
                "Preserve owner-only commits and tracked changes, ",
                "reconcile the base with origin, then retry delivery"
            )
            .into(),
            tool: None,
            input: None,
            command: format!(
                "git log --oneline '{}'",
                format!("origin/{branch}..{branch}").replace('\'', "'\\''")
            ),
            needs: vec![],
            also: vec![],
        },
        fingerprint: String::new(),
    };
    if item.age_seconds.is_some_and(|age| age >= 24 * 3600) {
        item.severity = AttentionSeverity::High;
    }
    item.fingerprint = cursor::attention_fingerprint(&item, detail);
    Ok(Some(item))
}

pub(super) fn read(
    context: &AttentionSourceContext<'_>,
    operator: bool,
    candidates: &mut AttentionCandidates,
) {
    if !context.filter.selected(KIND) {
        return;
    }
    if !operator {
        candidates.degraded.push(workspace_degraded(
            context.workspace,
            KIND.as_str(),
            "capability",
            "This runtime source requires the Operator capability",
        ));
        return;
    }
    if !context.runtime.stored().task().reads_visible() {
        candidates.degraded.push(workspace_degraded(
            context.workspace,
            KIND.as_str(),
            "source_unavailable",
            "Owner-base observation requires the workspace owner",
        ));
        return;
    }
    let start = Instant::now();
    let result: Result<Option<AttentionItem>, OrbitError> =
        (|| {
            let mut runs = Vec::new();
            for job in DELIVERY_LEAF_JOBS {
                runs.extend(context.runtime.stores().jobs().list_job_runs_filtered(
                    &JobRunQuery {
                        job_id: Some((*job).into()),
                        ..Default::default()
                    },
                )?);
            }
            runs.retain(|run| {
                run.executed_on
                    .as_ref()
                    .is_none_or(|host| host.machine_id == context.host.machine_id)
            });
            // Inspect the newest hold per base. A resolved newer base must not
            // hide an older, still-held base in the same workspace.
            runs.sort_by_key(|run| std::cmp::Reverse(run.recency_at()));
            let mut seen = std::collections::BTreeSet::new();
            for run in &runs {
                let Some((branch, checkout)) = hold(run) else {
                    continue;
                };
                if !seen.insert(branch) {
                    continue;
                }
                let admitted = runs.iter().any(|later| {
                    later.recency_at() > run.recency_at()
                        && later
                            .input
                            .as_ref()
                            .and_then(|input| input["base_branch"].as_str())
                            .is_some_and(|base| {
                                base.strip_prefix("origin/").unwrap_or(base) == branch
                            })
                        && later.steps.iter().any(|step| {
                            step.target_id == "worktree_setup" && step.state == JobRunState::Success
                        })
                });
                if !admitted && let Some(item) = observe(context, run, branch, checkout)? {
                    return Ok(Some(item));
                }
            }
            Ok(None)
        })();
    candidates
        .timings
        .insert(KIND.as_str().into(), start.elapsed().as_micros());
    if start.elapsed().as_secs() >= super::ATTENTION_SOURCE_TIMEOUT_SECONDS {
        candidates.degraded.push(workspace_degraded(
            context.workspace,
            KIND.as_str(),
            "source_timeout",
            "Runtime source read budget exhausted",
        ));
        return;
    }
    match result {
        Ok(Some(item)) => candidates.items.push(item),
        Ok(None) => {}
        Err(error) => candidates.degraded.push(workspace_degraded(
            context.workspace,
            KIND.as_str(),
            "source_unavailable",
            error.to_string(),
        )),
    }
}
