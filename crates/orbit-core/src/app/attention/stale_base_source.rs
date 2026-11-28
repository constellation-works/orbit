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

struct ObservationFailure {
    branch: String,
    checkout: Option<String>,
    message: String,
}

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
    let repo_path = &context.runtime.paths().repo_root;
    let repo = git2::Repository::open(repo_path).map_err(|error| {
        OrbitError::Execution(format!(
            "Cannot open workspace repository '{}': {error}",
            repo_path.display()
        ))
    })?;
    let resolve = |name: &str| {
        repo.find_reference(name)
            .and_then(|r| r.peel_to_commit())
            .map(|c| c.id())
            .map_err(|error| {
                OrbitError::Execution(format!("Cannot resolve Git ref {name}: {error}"))
            })
    };
    let owner_ref = format!("refs/heads/{branch}");
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let owner = resolve(&owner_ref)?;
    let remote = resolve(&remote_ref)?;
    let (ahead, behind) = repo.graph_ahead_behind(owner, remote).map_err(|error| {
        OrbitError::Execution(format!(
            "Cannot compare local and origin history for {branch}: {error}"
        ))
    })?;
    let dirty = if ahead == 0 && behind > 0 {
        if let Some(path) = checkout {
            let holder = git2::Repository::open(path).map_err(|error| {
                OrbitError::Execution(format!("Cannot open recorded checkout '{}': {error}", path))
            })?;
            let attached = holder
                .head()
                .map_err(|error| {
                    OrbitError::Execution(format!(
                        "Cannot read HEAD for recorded checkout '{}': {error}",
                        path
                    ))
                })?
                .shorthand()
                .ok()
                .map(str::to_owned);
            let mut options = git2::StatusOptions::new();
            options.include_untracked(false);
            attached.as_deref() == Some(branch)
                && !holder
                    .statuses(Some(&mut options))
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "Cannot read tracked status for recorded checkout '{}': {error}",
                            path
                        ))
                    })?
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
    let mut walk = repo.revwalk().map_err(|error| {
        OrbitError::Execution(format!("Cannot read commits for {branch}: {error}"))
    })?;
    walk.push(owner)
        .and_then(|()| walk.hide(remote))
        .map_err(|error| {
            OrbitError::Execution(format!("Cannot compare commits for {branch}: {error}"))
        })?;
    let mut commits = Vec::new();
    for oid in walk {
        let commit = repo
            .find_commit(oid.map_err(|error| {
                OrbitError::Execution(format!("Cannot read commit history for {branch}: {error}"))
            })?)
            .map_err(|error| {
                OrbitError::Execution(format!("Cannot read commit in {branch}: {error}"))
            })?;
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
    let result: Result<(Option<AttentionItem>, Vec<ObservationFailure>), OrbitError> =
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
            let mut observation_failures = Vec::new();
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
                if !admitted {
                    match observe(context, run, branch, checkout) {
                        Ok(Some(item)) => return Ok((Some(item), observation_failures)),
                        Ok(None) => {}
                        Err(error) => observation_failures.push(ObservationFailure {
                            branch: branch.to_owned(),
                            checkout: checkout.map(str::to_owned),
                            message: error.to_string(),
                        }),
                    }
                }
            }
            Ok((None, observation_failures))
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
        Ok((item, observation_failures)) => {
            for failure in observation_failures {
                let checkout = failure
                    .checkout
                    .map(|path| format!(" at recorded checkout '{path}'"))
                    .unwrap_or_default();
                let mut degraded = workspace_degraded(
                    context.workspace,
                    KIND.as_str(),
                    "source_unavailable",
                    format!(
                        "Cannot inspect held delivery base '{}'{}: {}. Verify its refs and recorded checkout, then retry",
                        failure.branch, checkout, failure.message
                    ),
                );
                degraded.subject = Some(failure.branch);
                candidates.degraded.push(degraded);
            }
            if let Some(item) = item {
                candidates.items.push(item);
            }
        }
        Err(error) => candidates.degraded.push(workspace_degraded(
            context.workspace,
            KIND.as_str(),
            "source_unavailable",
            error.to_string(),
        )),
    }
}
