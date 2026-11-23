//! Provider and record sources: `ci_red` from the provider read behind
//! `github.run.list`, cached per workspace and bounded by the source timeout.
use super::sources::{
    AttentionCandidates, AttentionSourceContext, SharedOpenTasks, attention_reason, attention_task,
    workspace_degraded,
};
use crate::OrbitRuntime;
use crate::application::contracts::ToolInvocationPort;
use chrono::{DateTime, Utc};
use orbit_common::governance::attention::cursor;
use orbit_types::tool::{
    AttentionItem, AttentionKind, AttentionNext, AttentionSeverity, AttentionTask,
    ToolSessionContext,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

struct CachedRuns {
    fetched_at: Instant,
    base_branch: String,
    head_sha: String,
    runs: Vec<Value>,
}

fn ci_cache() -> &'static Mutex<HashMap<String, CachedRuns>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedRuns>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn value_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => String::new(),
    }
}

fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.with_timezone(&Utc))
}

/// The workspace's git remote, when one is recognisable. A workspace with
/// no remote (or no repository) contributes no `ci_red` and no degradation.
fn workspace_remote(runtime: &OrbitRuntime) -> Option<String> {
    let repo = git2::Repository::open(runtime.paths().repo_root.as_path()).ok()?;
    let remote = repo.config().ok()?.get_string("remote.origin.url").ok()?;
    let remote = remote.trim().to_string();
    if remote.is_empty() {
        return None;
    }
    // Recognisable: a URL, an scp-like remote, or a path-like remote.
    if remote.contains("://") || remote.contains('@') || remote.contains('/') {
        Some(remote)
    } else {
        None
    }
}

/// Current head SHA of the base branch, from the fetched remote-tracking
/// ref `refs/remotes/origin/<base_branch>`. The caller only reaches here
/// when an origin remote is configured, so that ref is the authoritative
/// observation of the branch head: a local development branch can lag
/// behind it and must not shadow it. Empty when the remote-tracking ref
/// has not been fetched; the caller degrades explicitly instead of
/// treating a local or checked-out head as the current base head.
fn base_head_sha(runtime: &OrbitRuntime, base_branch: &str) -> String {
    let Ok(repo) = git2::Repository::open(runtime.paths().repo_root.as_path()) else {
        return String::new();
    };
    repo.find_reference(&format!("refs/remotes/origin/{base_branch}"))
        .ok()
        .and_then(|reference| reference.target())
        .map(|oid| oid.to_string())
        .unwrap_or_default()
}

fn call_provider(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    base_branch: &str,
    timeout: Duration,
) -> Result<Vec<Value>, String> {
    // Bound the whole provider read by the source timeout: run it on a
    // worker and stop waiting at the deadline. The underlying `gh` call
    // has its own 15s ceiling and finishes on its own afterwards.
    let runtime = runtime.clone();
    let session = session.clone();
    let branch = base_branch.to_string();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("attention-ci-read".into())
        .spawn(move || {
            let result = (|| -> Result<Vec<Value>, String> {
                let auth = runtime
                    .run_tool_command_with_session(
                        "github.auth.status",
                        json!({}),
                        None,
                        None,
                        session.clone(),
                    )
                    .map_err(|error| error.to_string())?;
                if auth["available"] != json!(true) || auth["authenticated"] != json!(true) {
                    return Err("GitHub authentication missing".to_string());
                }
                let listed = runtime
                    .run_tool_command_with_session(
                        "github.run.list",
                        json!({"branch": branch, "limit": 100}),
                        None,
                        None,
                        session,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(listed["runs"].as_array().cloned().unwrap_or_default())
            })();
            let _ = sender.send(result);
        });
    match spawned {
        Ok(_) => match receiver.recv_timeout(timeout) {
            Ok(result) => result,
            Err(_) => Err("GitHub provider read timed out".to_string()),
        },
        Err(error) => Err(error.to_string()),
    }
}

/// The repair-task lookup for one query: CI run id → the first open task
/// (in listing order) whose creation identity names that run. Built once
/// — one event-log read per open task — and reused for every red
/// workflow, replacing the former per-workflow scan (R × M history reads).
/// History reads settle a pending task commit, so the caller never builds
/// this while one is pending; a shared-listing failure yields an empty
/// lookup, as the former per-workflow read's failure yielded no repair.
fn build_repair_lookup(
    runtime: &OrbitRuntime,
    open_tasks: &SharedOpenTasks,
) -> BTreeMap<String, (String, Option<AttentionTask>)> {
    use orbit_types::task::CiFailureIdentity;
    let mut lookup = BTreeMap::new();
    let Ok(tasks) = open_tasks.get(runtime) else {
        return lookup;
    };
    for task in tasks {
        if task.status.is_terminal() {
            continue;
        }
        let Ok(history) = runtime.get_task_history(&task.id) else {
            continue;
        };
        let Ok(Some(identity)) = CiFailureIdentity::from_history(&history) else {
            continue;
        };
        for source in &identity.sources {
            lookup
                .entry(source.run_id.clone())
                .or_insert_with(|| (task.id.clone(), Some(attention_task(task))));
        }
    }
    lookup
}

/// A run's timestamp from `primary`, falling back to `secondary`, else the
/// epoch so an undated run never outranks a dated one.
fn run_time(run: &Value, primary: &str, secondary: &str) -> DateTime<Utc> {
    parse_time(&run[primary])
        .or_else(|| parse_time(&run[secondary]))
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// The run that decides each workflow's CI state on the base branch, keyed
/// by workflow (or run id when the workflow is unnamed).
///
/// A workflow with a completed run on the current base head is decided by
/// its newest such run (by `updated_at`). A workflow whose only runs on the
/// current head are still pending is decided by its newest completed run
/// on the base branch (by `created_at`, the push order), whatever head it
/// ran on: a base that keeps landing always has a pending run on its newest
/// head, and a red result must not vanish behind it, while a newer green
/// completion still clears it. A workflow with no run on the current head
/// is not decided: an older head's result alone is not current.
fn newest_red_candidates(
    runs: &[Value],
    base_branch: &str,
    head_sha: &str,
) -> BTreeMap<String, Value> {
    let workflow_key = |run: &Value| {
        let workflow = value_string(&run["workflow"]);
        if workflow.is_empty() {
            value_string(&run["run_id"])
        } else {
            workflow
        }
    };
    let mut on_head: BTreeMap<String, Value> = BTreeMap::new();
    let mut pending_on_head: std::collections::BTreeSet<String> = Default::default();
    let mut on_branch: BTreeMap<String, Value> = BTreeMap::new();
    for run in runs {
        if value_string(&run["head_branch"]) != base_branch {
            continue;
        }
        let reported = value_string(&run["reported_head_sha"]);
        let current = !reported.is_empty() && reported == head_sha;
        let status = value_string(&run["status"]);
        let key = workflow_key(run);
        if status != "completed" {
            if current && !status.is_empty() {
                pending_on_head.insert(key);
            }
            continue;
        }
        if value_string(&run["conclusion"]).is_empty() {
            continue;
        }
        if current {
            let newer = on_head.get(&key).is_none_or(|existing| {
                run_time(run, "updated_at", "created_at")
                    > run_time(existing, "updated_at", "created_at")
            });
            if newer {
                on_head.insert(key.clone(), run.clone());
            }
        }
        let newer = on_branch.get(&key).is_none_or(|existing| {
            (
                run_time(run, "created_at", "updated_at"),
                run_time(run, "updated_at", "created_at"),
            ) > (
                run_time(existing, "created_at", "updated_at"),
                run_time(existing, "updated_at", "created_at"),
            )
        });
        if newer {
            on_branch.insert(key, run.clone());
        }
    }
    for key in pending_on_head {
        if on_head.contains_key(&key) {
            continue;
        }
        if let Some(run) = on_branch.remove(&key) {
            on_head.insert(key, run);
        }
    }
    on_head
}

pub(super) fn read_ci_attention(
    context: &AttentionSourceContext<'_>,
    session: &ToolSessionContext,
    candidates: &mut AttentionCandidates,
    open_tasks: &SharedOpenTasks,
) {
    let runtime = context.runtime;
    let workspace = context.workspace;
    let host = context.host;
    let filter = context.filter;
    let now = context.now;
    if !filter.selected(AttentionKind::CiRed) {
        return;
    }
    let started = Instant::now();
    // ci_red is derived only on the home owner: a replica must not read the
    // provider, and its local CI state is not authoritative.
    if !runtime.coordination_task_reads_visible() {
        if !filter.candidate_read {
            candidates.degraded.push(workspace_degraded(
                workspace,
                AttentionKind::CiRed.as_str(),
                "source_unavailable",
                "CI results on a replica are not authoritative; read the workspace owner",
            ));
        }
        candidates
            .timings
            .insert("ci_red".into(), started.elapsed().as_micros());
        return;
    }
    if workspace_remote(runtime).is_none() {
        return;
    }
    let base_branch = runtime.workspace_base_branch().to_string();
    let head_sha = base_head_sha(runtime, &base_branch);
    if head_sha.is_empty() {
        // No fetched remote-tracking head: freshness is unknown, so no
        // run can be shown to be on the current base head. Degrade
        // explicitly rather than treating any reported head as current.
        candidates.degraded.push(workspace_degraded(
            workspace,
            AttentionKind::CiRed.as_str(),
            "source_unavailable",
            format!(
                "Current head of base branch '{base_branch}' is unavailable; fetch the branch and retry"
            ),
        ));
        candidates
            .timings
            .insert("ci_red".into(), started.elapsed().as_micros());
        return;
    }
    // Cache per workspace, keyed by the freshness inputs.
    let cache_key = workspace.id.clone();
    let cached_runs = {
        let cache = ci_cache().lock().unwrap_or_else(|p| p.into_inner());
        cache.get(&cache_key).and_then(|entry| {
            if entry.base_branch == base_branch
                && entry.head_sha == head_sha
                && entry.fetched_at.elapsed()
                    < Duration::from_secs(super::ATTENTION_CI_CACHE_SECONDS)
            {
                Some(entry.runs.clone())
            } else {
                None
            }
        })
    };
    let runs = match cached_runs {
        Some(runs) => runs,
        None => {
            let timeout = Duration::from_secs(super::ATTENTION_SOURCE_TIMEOUT_SECONDS);
            match call_provider(runtime, session, &base_branch, timeout) {
                Ok(runs) => {
                    if let Ok(mut cache) = ci_cache().lock() {
                        cache.insert(
                            cache_key,
                            CachedRuns {
                                fetched_at: Instant::now(),
                                base_branch: base_branch.clone(),
                                head_sha: head_sha.clone(),
                                runs: runs.clone(),
                            },
                        );
                    }
                    runs
                }
                Err(message) => {
                    candidates.degraded.push(workspace_degraded(
                        workspace,
                        AttentionKind::CiRed.as_str(),
                        "source_unavailable",
                        message,
                    ));
                    candidates
                        .timings
                        .insert("ci_red".into(), started.elapsed().as_micros());
                    return;
                }
            }
        }
    };
    let newest_by_workflow = newest_red_candidates(&runs, &base_branch, &head_sha);
    // A pending task commit makes the repair-task association
    // unavailable: history reads would settle it. The CI evidence itself
    // is still safe to report, degraded explicitly below.
    let pending_commit = super::sources::pending_commit_message(runtime);
    let mut repair_lookup_degraded = false;
    let mut repair_lookup: Option<BTreeMap<String, (String, Option<AttentionTask>)>> = None;
    for (workflow_key, run) in newest_by_workflow {
        let conclusion = value_string(&run["conclusion"]);
        if conclusion != "failure" && conclusion != "timed_out" {
            continue;
        }
        if !repair_lookup_degraded && let Some(message) = &pending_commit {
            repair_lookup_degraded = true;
            candidates.degraded.push(workspace_degraded(
                workspace,
                AttentionKind::CiRed.as_str(),
                "source_unavailable",
                format!("CI repair-task lookup unavailable: {message}"),
            ));
        }
        let run_id = value_string(&run["run_id"]);
        // `since` is the failing run's `updated_at`, the nearest
        // completion-time proxy the provider projection supplies.
        let Some(since) = parse_time(&run["updated_at"]).or_else(|| parse_time(&run["created_at"]))
        else {
            continue;
        };
        let age = (now - since).num_seconds().max(0) as u64;
        let critical = age >= u64::from(super::ATTENTION_AGE_BUMP_HOURS[1]) * 3600;
        let severity = if critical {
            AttentionSeverity::Critical
        } else {
            AttentionSeverity::High
        };
        let workflow = if workflow_key.is_empty() {
            run_id.clone()
        } else {
            workflow_key.clone()
        };
        let repair = if run_id.is_empty() || pending_commit.is_some() {
            None
        } else {
            let lookup =
                repair_lookup.get_or_insert_with(|| build_repair_lookup(runtime, open_tasks));
            lookup.get(&run_id).cloned()
        };
        let (next, refs_task, task_meta) = if let Some((task_id, task_meta)) = &repair {
            (
                AttentionNext {
                    summary: "Inspect the CI repair task".into(),
                    tool: Some("orbit.task.show".into()),
                    input: Some(json!({"id": task_id, "workspace": workspace.id})),
                    command: format!(
                        "orbit --workspace '{}' task show {}",
                        workspace.id.replace('\'', "'\\''"),
                        task_id
                    ),
                    needs: Vec::new(),
                    also: Vec::new(),
                },
                Some(task_id.clone()),
                task_meta.clone(),
            )
        } else {
            (
                AttentionNext {
                    summary: "Inspect the failing run".into(),
                    tool: Some("github.run.view".into()),
                    input: Some(json!({"run": run_id})),
                    command: format!(
                        "orbit tool run github.run.view --input '{{\"run\":\"{run_id}\"}}'"
                    ),
                    needs: Vec::new(),
                    also: Vec::new(),
                },
                None,
                None,
            )
        };
        let mut refs = json!({"run_id": run_id});
        if let Some(task_id) = refs_task {
            refs["task_id"] = json!(task_id);
        }
        let title = value_string(&run["title"]);
        let reason = if title.is_empty() {
            format!("CI workflow {workflow} failed on {base_branch}")
        } else {
            title
        };
        let mut item = AttentionItem {
            id: cursor::attention_item_id(
                AttentionKind::CiRed,
                &host.machine_id,
                &workspace.id,
                &json!(workflow),
            ),
            kind: AttentionKind::CiRed,
            severity,
            workspace: workspace.clone(),
            host: host.clone(),
            refs,
            task: task_meta,
            since: Some(since),
            age_seconds: Some(age),
            expires_at: None,
            reason: attention_reason(&reason),
            detail: json!({
                "workflow": workflow,
                "conclusion": conclusion,
                "branch": base_branch,
                "reported_head_sha": value_string(&run["reported_head_sha"]),
                "base_head_sha": head_sha,
                "url": run["url"],
                "since_source": "updated_at",
                "since_is_proxy": true,
            }),
            next,
            fingerprint: String::new(),
        };
        item.fingerprint = cursor::attention_fingerprint(
            &item,
            json!({"workflow": workflow, "run_id": run_id, "conclusion": conclusion}),
        );
        candidates.items.push(item);
    }
    candidates
        .timings
        .insert("ci_red".into(), started.elapsed().as_micros());
}
