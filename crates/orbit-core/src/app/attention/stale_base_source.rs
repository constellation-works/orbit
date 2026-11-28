//! Read-only observation of owner-base setup refusals.

use super::source::{
    AttentionCandidates, AttentionSourceContext, attention_reason, workspace_degraded,
};
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::attention::cursor;
use orbit_engine::BaseSyncMode;
use orbit_store::contract::{JobRunInputTextFilter, JobRunStepTargetQuery};
use orbit_types::tool::{AttentionItem, AttentionKind, AttentionNext, AttentionSeverity};
use orbit_types::workflow::{
    DELIVERY_LEAF_JOBS, JobRun, JobRunState, JobRunStep, JobTargetType,
    TASK_CLAIMED_LOCAL_PIPELINE_JOB, Trailer, TrailerKey, WORKTREE_SETUP_STEP_ID,
};
use serde_json::json;
use std::time::Instant;

const KIND: AttentionKind = AttentionKind::DeliveryStaleBase;

const OWNER_ONLY_COMMIT_LIMIT: usize = 20;

const STALE_BASE_HOLD_QUERY_LIMIT: usize = 128;

const STALE_BASE_CLEAR_QUERY_LIMIT: usize = 64;

const STALE_BASE_CLEAR_LOOKBACK_MINUTES: i64 = 5;

const STALE_BASE_INPUT_KEYS: &[&str] = &["base_sync", "base_branch"];

struct Hold<'a> {
    branch: &'a str,
    checkout: Option<&'a str>,
    at: DateTime<Utc>,
}

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

fn step_at(step: &JobRunStep) -> Option<DateTime<Utc>> {
    step.finished_at.or(step.started_at)
}

fn hold(run: &JobRun) -> Option<Hold<'_>> {
    run.steps
        .iter()
        .rev()
        .filter(|step| step.target_id == WORKTREE_SETUP_STEP_ID)
        .find_map(|step| {
            let (branch, checkout) = refusal(step.error_message.as_deref()?)?;
            Some(Hold {
                branch,
                checkout,
                at: step_at(step).unwrap_or_else(|| run.recency_at()),
            })
        })
}

fn uses_local_base_sync(run: &JobRun) -> bool {
    let explicit = run
        .input
        .as_ref()
        .and_then(|input| input.get("base_sync"))
        .and_then(serde_json::Value::as_str);
    match explicit {
        Some(value) => value.parse::<BaseSyncMode>().ok() == Some(BaseSyncMode::Local),
        None => run.job_id == TASK_CLAIMED_LOCAL_PIPELINE_JOB,
    }
}

fn setup_admitted_after_hold(later: &JobRun, hold: &Hold<'_>) -> bool {
    if !uses_local_base_sync(later) {
        return false;
    }
    let same_branch = later
        .input
        .as_ref()
        .and_then(|input| input["base_branch"].as_str())
        .is_some_and(|base| base.strip_prefix("origin/").unwrap_or(base) == hold.branch);
    if !same_branch {
        return false;
    }
    later.steps.iter().any(|step| {
        if step.target_id != WORKTREE_SETUP_STEP_ID || step.state != JobRunState::Success {
            return false;
        }
        let Some(finished_at) = step_at(step) else {
            return false;
        };
        let started_at = step.started_at.unwrap_or(finished_at);
        started_at >= hold.at && finished_at > hold.at
    })
}

fn retain_local_host_runs(runs: &mut Vec<JobRun>, machine_id: &str) {
    runs.retain(|run| {
        run.executed_on
            .as_ref()
            .is_none_or(|host| host.machine_id == machine_id)
    });
}

fn clearing_query(
    job_ids: Vec<String>,
    allow_missing_base_sync: bool,
    branch_values: &[String],
    state: Option<JobRunState>,
    recency_since: Option<DateTime<Utc>>,
) -> JobRunStepTargetQuery {
    JobRunStepTargetQuery {
        job_ids,
        state,
        recency_since,
        step_target_id: WORKTREE_SETUP_STEP_ID.into(),
        step_target_type: Some(JobTargetType::Activity),
        step_state: Some(JobRunState::Success),
        input_text_filters: vec![
            JobRunInputTextFilter {
                key: "base_sync",
                values: vec!["local".into()],
                trim: true,
                allow_missing: allow_missing_base_sync,
            },
            JobRunInputTextFilter {
                key: "base_branch",
                values: branch_values.to_vec(),
                trim: false,
                allow_missing: false,
            },
        ],
        input_keys: Some(STALE_BASE_INPUT_KEYS),
        limit: Some(STALE_BASE_CLEAR_QUERY_LIMIT),
        ..Default::default()
    }
}

fn has_admitted_local_setup_after_hold(
    context: &AttentionSourceContext<'_>,
    hold: &Hold<'_>,
) -> Result<bool, OrbitError> {
    let jobs = context.runtime.stores().jobs();
    let branch_values = [hold.branch.to_owned(), format!("origin/{}", hold.branch)];
    let explicit_local_jobs = DELIVERY_LEAF_JOBS
        .iter()
        .copied()
        .filter(|job| *job != TASK_CLAIMED_LOCAL_PIPELINE_JOB)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let default_local_jobs = vec![TASK_CLAIMED_LOCAL_PIPELINE_JOB.to_owned()];
    let recent_since = Some(hold.at - Duration::minutes(STALE_BASE_CLEAR_LOOKBACK_MINUTES));
    for (job_ids, allow_missing) in [(explicit_local_jobs, false), (default_local_jobs, true)] {
        for (state, recency_since) in [(None, recent_since), (Some(JobRunState::Running), None)] {
            let mut candidates = jobs.list_job_runs_with_step_target(&clearing_query(
                job_ids.clone(),
                allow_missing,
                &branch_values,
                state,
                recency_since,
            ))?;
            retain_local_host_runs(&mut candidates, &context.host.machine_id);
            if candidates
                .iter()
                .any(|later| setup_admitted_after_hold(later, hold))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
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
    let dirty = if behind > 0 {
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
    for oid in walk.take(OWNER_ONLY_COMMIT_LIMIT) {
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
            "task_id": task_from_message(message).map(str::to_owned).or_else(|| {
                commit.message().ok().and_then(|full| {
                    full.lines().skip(1).filter_map(Trailer::parse_line)
                        .find(|trailer| trailer.key == TrailerKey::OrbitTask)
                        .map(|trailer| trailer.value)
                })
            }),
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
    let reason = if ahead == 0 {
        format!(
            "Deliveries held: {branch} is behind {behind} of origin/{branch}; checkout has tracked changes"
        )
    } else {
        format!(
            "Deliveries held: {branch} is ahead {ahead}, behind {behind} of origin/{branch}; owner-only commit {commit_hint}{task_hint}"
        )
    };
    let command = if ahead == 0 {
        let path = checkout.unwrap_or(".").replace('\'', "'\\''");
        format!("git -C '{path}' status --short --untracked-files=no")
    } else {
        format!(
            "git log --oneline '{}'",
            format!("origin/{branch}..{branch}").replace('\'', "'\\''")
        )
    };
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
        reason: attention_reason(&reason),
        detail: detail.clone(),
        next: AttentionNext {
            summary: concat!(
                "Preserve owner-only commits and tracked changes, ",
                "reconcile the base with origin, then retry delivery"
            )
            .into(),
            tool: None,
            input: None,
            command,
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
        // A replica is not authoritative for the owner base. The aggregator's
        // candidate read asks the owner, so the replica stays silent; a direct
        // read says where to look.
        if !context.filter.candidate_read {
            candidates.degraded.push(workspace_degraded(
                context.workspace,
                KIND.as_str(),
                "source_unavailable",
                "Owner-base observation requires the workspace owner",
            ));
        }
        return;
    }
    let start = Instant::now();
    let result: Result<(Option<AttentionItem>, Vec<ObservationFailure>), OrbitError> = (|| {
        let mut held_runs = context
            .runtime
            .stores()
            .jobs()
            .list_job_runs_with_step_target(&JobRunStepTargetQuery {
                job_ids: DELIVERY_LEAF_JOBS
                    .iter()
                    .map(|job| (*job).to_owned())
                    .collect(),
                state: Some(JobRunState::Held),
                step_target_id: WORKTREE_SETUP_STEP_ID.into(),
                step_target_type: Some(JobTargetType::Activity),
                step_exclude_state: Some(JobRunState::Success),
                step_require_error_message: true,
                input_keys: Some(&[]),
                limit: Some(STALE_BASE_HOLD_QUERY_LIMIT),
                ..Default::default()
            })?;
        retain_local_host_runs(&mut held_runs, &context.host.machine_id);
        // Inspect the newest hold per base. A resolved newer base must not
        // hide an older, still-held base in the same workspace.
        held_runs.sort_by_key(|run| std::cmp::Reverse(run.recency_at()));
        let mut seen = std::collections::BTreeSet::new();
        let mut observation_failures = Vec::new();
        for run in &held_runs {
            let Some(hold) = hold(run) else {
                continue;
            };
            if !seen.insert(hold.branch) {
                continue;
            }
            if !has_admitted_local_setup_after_hold(context, &hold)? {
                match observe(context, run, hold.branch, hold.checkout) {
                    Ok(Some(item)) => return Ok((Some(item), observation_failures)),
                    Ok(None) => {}
                    Err(error) => observation_failures.push(ObservationFailure {
                        branch: hold.branch.to_owned(),
                        checkout: hold.checkout.map(str::to_owned),
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
