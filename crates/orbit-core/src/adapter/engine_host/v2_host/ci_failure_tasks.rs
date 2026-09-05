//! `file_ci_failure_tasks` — turn one CI evidence snapshot into quarantined
//! proposed tasks.
//!
//! The snapshot arrives from the host-owned `collect_ci_evidence` step, which
//! ran `gh` outside any agent sandbox. Everything below is a pure function of
//! that JSON plus the workspace's open tasks: cluster the current failures by
//! root cause, drop the clusters a still-open task already covers, and file
//! what is left as `proposed` bug tasks whose descriptions carry the evidence
//! inline. The CI sweep then pilots and revalidates those tasks before a
//! separate admission boundary may expose them to backlog auto-drain.
//!
//! A filed task deliberately has no `required_tools`: the pilot and eventual
//! implementation never have to query GitHub, because the original evidence
//! is already in the description. It is not executable until a successful
//! pilot has applied valid selectors, found the failure relevant to current
//! integration code, and exercised explicit sweep promotion authority.
//!
//! # Two keys, on purpose
//!
//! `cluster_key` includes the commit the runner actually tested, so one
//! regression observed across a push run and a pull-request run of the *same*
//! commit collapses into one task instead of two.
//!
//! `failure_key` — the dedupe tag — deliberately omits the commit. The sweep is
//! hourly and a fix takes longer than that; keying dedupe on the commit would
//! file the same root cause again every time the branch advanced, which is the
//! backlog flood this step exists to prevent.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::duplicate_tasks::{
    CoverageAnchor, CoverageFingerprint, DuplicateCandidate, DuplicateTaskLookup,
    DuplicateTaskMatch, find_covering_task,
};
use crate::application::task::TaskAddParams;

/// Wire contract with `collect_ci_evidence` (`orbit-engine`'s
/// `executor::automation::ci`), also stated in both activity assets' schemas.
/// The three endings must never collapse into one another, and none of them is
/// a CI pass.
const OUTCOME_CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
const OUTCOME_NO_CURRENT_FAILURE: &str = "no_current_failure";
const OUTCOME_CURRENT_FAILURES: &str = "current_failures";

/// Snapshot schema this step knows how to read.
const SUPPORTED_SCHEMA_VERSION: u64 = 1;

/// Provenance tag: every task this step files carries it.
pub(crate) const CI_FAILURE_TAG: &str = "ci-failure-sweep";
/// Prefix of the dedupe tag, completed by the failure key.
pub(crate) const CI_FAILURE_KEY_TAG_PREFIX: &str = "ci-failure:";
/// Title prefix on every task this step files, so a sweep-filed task is
/// identifiable in a backlog listing without reading its tags.
const CI_FAILURE_SWEEP_TITLE_PREFIX: &str = "[ci-failure-sweep] ";
/// The system crew name. Filed tasks belong on the system lane, matching the
/// shipped `ci-failure-remediation` auto-task — but this is a plain default,
/// not a hard-coded assumption that the lane is configured: a workspace whose
/// crew roster has no `system` entry still gets its task filed, just without
/// a crew set.
const SYSTEM_CREW: &str = "system";

const DEFAULT_MAX_TASKS: u64 = 5;
const MAX_MAX_TASKS: u64 = 20;
/// Hex characters of the signature digest kept in a tag. Full-width digests
/// make a tag unreadable in a task list; this is a dedupe key, not a security
/// boundary.
const KEY_LEN: usize = 16;
/// Log bytes carried into a task description. `collect_ci_evidence` has already
/// bounded and redacted the excerpt; this is a second, tighter bound so a
/// description stays a readable brief.
const DESCRIPTION_LOG_BYTES: usize = 4_000;
/// Runs listed per cluster in the description.
const MAX_LISTED_RUNS: usize = 6;

pub(crate) fn file_ci_failure_tasks(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<Value, OrbitError> {
    file_ci_failure_tasks_with_ops(runtime, input, runtime, |params| {
        runtime.add_task(params).map(|task| task.id)
    })
}

#[cfg(test)]
pub(in crate::adapter::engine_host::v2_host) fn file_ci_failure_tasks_with_add<F>(
    runtime: &OrbitRuntime,
    input: &Value,
    add_task: F,
) -> Result<Value, OrbitError>
where
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    file_ci_failure_tasks_with_ops(runtime, input, runtime, add_task)
}

#[cfg(test)]
pub(in crate::adapter::engine_host::v2_host) fn file_ci_failure_tasks_with_lookup<L>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    file_ci_failure_tasks_with_ops(runtime, input, lookup, |params| {
        runtime.add_task(params).map(|task| task.id)
    })
}

fn file_ci_failure_tasks_with_ops<L, F>(
    runtime: &OrbitRuntime,
    input: &Value,
    lookup: &L,
    mut add_task: F,
) -> Result<Value, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
    F: FnMut(TaskAddParams) -> Result<String, OrbitError>,
{
    let evidence = input.get("ci_evidence").ok_or_else(|| {
        OrbitError::InvalidInput(
            "file_ci_failure_tasks requires the `ci_evidence` snapshot produced by \
             collect_ci_evidence"
                .to_string(),
        )
    })?;
    if !evidence.is_object() {
        return Err(OrbitError::InvalidInput(
            "file_ci_failure_tasks requires `ci_evidence` to be the snapshot object".to_string(),
        ));
    }
    let schema_version = evidence
        .get("schema_version")
        .and_then(Value::as_u64)
        .unwrap_or(SUPPORTED_SCHEMA_VERSION);
    if schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(OrbitError::InvalidInput(format!(
            "ci_evidence schema version {schema_version} is newer than the supported version \
             {SUPPORTED_SCHEMA_VERSION}; refusing to file tasks from a snapshot this step \
             cannot read"
        )));
    }
    let capability = evidence.get("capability").cloned().unwrap_or(Value::Null);

    // The collection stage could not look. Every list below would be empty and
    // would read exactly like "nothing is failing" — a conclusion that needs
    // queries this host never ran.
    if evidence.get("collected").and_then(Value::as_bool) != Some(true) {
        let audit = audit_summary(evidence, &[]);
        return Ok(json!({
            "outcome": OUTCOME_CAPABILITY_UNAVAILABLE,
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidates": [],
            "skipped_existing": [],
            "skipped_over_cap": [],
            "deferred": [],
            "audit": audit,
            "detail": "no CI evidence was gathered, so no task was filed; this is not a CI pass",
        }));
    }

    let max_tasks = bounded_u64(input, "max_tasks", DEFAULT_MAX_TASKS, MAX_MAX_TASKS)? as usize;
    let failures = evidence
        .get("current_failures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let audit = audit_summary(evidence, &failures);
    let mut retryable_errors = evidence
        .get("retryable_errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Version-1 snapshots emitted `query_errors`. Treat them with the repaired
    // contract when an older collect step is still paired with this filer.
    retryable_errors.extend(
        evidence
            .get("query_errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );
    retryable_errors = retryable_errors
        .into_iter()
        .map(normalize_retryable_error)
        .collect();
    // A gap in one run's evidence is a fact about that run. Letting it also
    // withhold every complete finding in the same snapshot is how a sweep that
    // had three fully evidenced regressions in hand filed nothing at all.
    let (mut snapshot_wide, mut run_errors) = partition_retryable_errors(&retryable_errors);
    // An uninvestigated failure collection said nothing else about still needs
    // its own reason; one that already has a recorded cause keeps that cause
    // rather than being restated generically.
    for failure in &failures {
        if failure.get("investigated").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let run_id = run_id_key(failure);
        if run_id
            .as_ref()
            .is_some_and(|run_id| run_errors.contains_key(run_id))
        {
            continue;
        }
        let error = json!({
            "stage": "registration",
            "operation": "current_failure_not_investigated",
            "run_id": failure.get("run_id"),
            "retryable": true,
            "message": "a current CI failure has no complete investigation and cannot be filed safely",
        });
        retryable_errors.push(error.clone());
        match run_id {
            Some(run_id) => run_errors.entry(run_id).or_default().push(error),
            // Without a run ID there is nothing to defer *to*: the finding
            // cannot be told apart from any other, so the snapshot is unsafe
            // to file from at all.
            None => snapshot_wide += 1,
        }
    }
    if snapshot_wide > 0 {
        // A snapshot this incomplete cannot be filed from at all: the listing
        // that failed may be exactly the one holding the newer run that would
        // have superseded a finding. Report every error, not just the
        // snapshot-wide ones, so one payload explains the whole sweep.
        return Err(retryable_pipeline_error(
            "collection_or_investigation",
            &audit,
            retryable_errors,
        ));
    }
    let (complete, deferred) = split_deferred_failures(&failures, &run_errors);
    let audit = deferral_audit(audit, &deferred);
    let clusters = cluster_failures(&complete);

    if !complete.is_empty() && clusters.is_empty() {
        return Err(retryable_pipeline_error(
            "registration",
            &audit,
            vec![json!({
                "stage": "registration",
                "operation": "cluster_failures",
                "retryable": true,
                "message": "current failures were discovered but none could be registered for task filing",
            })],
        ));
    }

    if clusters.is_empty() {
        // Nothing was complete enough to file. The gaps are the whole result,
        // so this stays a retryable error rather than a clean sweep.
        if !deferred.is_empty() {
            return Err(retryable_pipeline_error(
                "collection_or_investigation",
                &audit,
                deferred_errors(&deferred),
            ));
        }
        return Ok(json!({
            "outcome": OUTCOME_NO_CURRENT_FAILURE,
            "capability": capability,
            "clusters": 0,
            "filed_count": 0,
            "filed": [],
            "pilot_candidates": [],
            "skipped_existing": [],
            "skipped_over_cap": [],
            "deferred": [],
            "audit": audit,
            "detail": "the queries ran and found no current, non-superseded failure",
        }));
    }

    let mut filed = Vec::new();
    let mut pilot_candidates = Vec::new();
    let mut pilot_candidate_ids = BTreeSet::new();
    let mut skipped_existing = Vec::new();
    let mut skipped_over_cap = Vec::new();
    // Two clusters in one snapshot can share a failure key when the same root
    // cause was tested at two commits. The first filing closes the second.
    let mut filed_keys: BTreeSet<String> = BTreeSet::new();
    // Probed once per sweep rather than assumed: a workspace whose crew
    // roster has no `system` entry still needs filing to succeed, degrading
    // the same way any other task with an unrecognized crew does instead of
    // failing the sweep.
    let system_crew = runtime
        .validate_crew_name(Some(SYSTEM_CREW))
        .is_ok()
        .then(|| SYSTEM_CREW.to_string());

    // Complete every external lookup before the first task write. A transient
    // duplicate-check failure must leave no partial filing or dedupe state.
    let duplicate_matches = clusters
        .iter()
        .map(|cluster| {
            find_covering_task(lookup, &cluster.duplicate_candidate()).map_err(|error| {
                retryable_pipeline_error(
                    "dedupe_lookup",
                    &audit,
                    vec![json!({
                        "stage": "registration",
                        "operation": "find_covering_task",
                        "failure_key": cluster.failure_key,
                        "retryable": true,
                        "message": bounded_error(&error.to_string()),
                    })],
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let duplicate_tasks = duplicate_matches
        .iter()
        .map(|duplicate_match| {
            duplicate_match
                .as_ref()
                .map(|matched| {
                    runtime.get_task(&matched.task_id).map_err(|error| {
                        retryable_pipeline_error(
                            "pilot_candidate_lookup",
                            &audit,
                            vec![json!({
                                "stage": "registration",
                                "operation": "reload_duplicate_task",
                                "task_id": matched.task_id,
                                "retryable": true,
                                "message": bounded_error(&error.to_string()),
                            })],
                        )
                    })
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;

    for ((cluster, duplicate_match), duplicate_task) in
        clusters.iter().zip(duplicate_matches).zip(duplicate_tasks)
    {
        if let Some(DuplicateTaskMatch {
            task_id,
            match_kind,
            evidence,
        }) = duplicate_match
        {
            if let Some(existing) = duplicate_task {
                let expected_key_tag =
                    format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key);
                if existing.status == TaskStatus::Proposed
                    && existing.tags.iter().any(|tag| tag == CI_FAILURE_TAG)
                    && existing.tags.iter().any(|tag| tag == &expected_key_tag)
                    && pilot_candidate_ids.insert(task_id.clone())
                {
                    pilot_candidates.push(cluster.filing_entry(&task_id));
                }
            }
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": task_id,
                "workflow": cluster.workflow,
                "match_kind": match_kind,
                "match_evidence": evidence,
            }));
            continue;
        }
        if filed_keys.contains(&cluster.failure_key) {
            skipped_existing.push(json!({
                "failure_key": cluster.failure_key,
                "cluster_key": cluster.cluster_key,
                "task_id": filed
                    .iter()
                    .find(|entry: &&Value| entry["failure_key"] == json!(cluster.failure_key))
                    .and_then(|entry| entry["task_id"].as_str())
                    .unwrap_or_default(),
                "workflow": cluster.workflow,
                "match_kind": "exact_key",
                "match_evidence": {
                    "fingerprint": "same_sweep_key",
                    "matched_fields": [{
                        "field": "failure_key",
                        "value": cluster.failure_key,
                    }],
                },
            }));
            continue;
        }
        if filed.len() >= max_tasks {
            skipped_over_cap.push(json!({
                "failure_key": cluster.failure_key,
                "workflow": cluster.workflow,
                "job": cluster.job,
                "run_urls": cluster.run_urls(),
            }));
            continue;
        }

        let task_id = add_task(TaskAddParams {
            title: cluster.title(),
            description: cluster.description(evidence),
            acceptance_criteria: cluster.acceptance_criteria(),
            tags: vec![
                CI_FAILURE_TAG.to_string(),
                format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", cluster.failure_key),
                "github-actions".to_string(),
            ],
            // Deliberately empty: the evidence is already in the description,
            // so the task ships on the ordinary agent baseline.
            required_tools: Vec::new(),
            crew: system_crew.clone(),
            priority: TaskPriority::High,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Bug),
            // Filing is quarantine, not dispatch authorization. The
            // task-pilot apply boundary is the only CI-sweep path that may
            // promote a relevant, selector-backed repair to backlog.
            status: Some(TaskStatus::Proposed),
            system_created: true,
            ..TaskAddParams::default()
        })
        .map_err(|error| {
            retryable_pipeline_error(
                "task_creation",
                &audit,
                vec![json!({
                    "stage": "task_creation",
                    "operation": "orbit.task.add",
                    "failure_key": cluster.failure_key,
                    "run_ids": cluster.run_ids(),
                    "retryable": true,
                    "message": bounded_error(&error.to_string()),
                })],
            )
        })?;
        filed_keys.insert(cluster.failure_key.clone());
        let filing = cluster.filing_entry(&task_id);
        pilot_candidate_ids.insert(task_id);
        pilot_candidates.push(filing.clone());
        filed.push(filing);
    }

    let final_audit = filing_audit(audit, &filed, &skipped_existing);
    Ok(json!({
        "outcome": OUTCOME_CURRENT_FAILURES,
        "capability": capability,
        "clusters": clusters.len(),
        "filed_count": filed.len(),
        "filed": filed,
        "pilot_candidates": pilot_candidates,
        "skipped_existing": skipped_existing,
        "skipped_over_cap": skipped_over_cap,
        "deferred": deferred,
        "max_tasks": max_tasks,
        "audit": final_audit,
    }))
}

/// The run a snapshot entry — a retryable error or a current failure — is
/// about, as a comparable key. Collection emits a numeric `run_id`; the
/// version-1 `query_errors` shape used a string.
fn run_id_key(entry: &Value) -> Option<String> {
    match entry.get("run_id") {
        Some(Value::Number(number)) => Some(number.to_string()),
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

/// Split retryable errors by blast radius.
///
/// An error that names a run spoils that run's finding and nothing else. An
/// error that names none — a repository read, a run listing, a pull-request
/// listing — leaves the whole snapshot in doubt: any finding it did produce
/// could be missing the newer run that would have superseded it, so filing
/// from that snapshot is not safe.
fn partition_retryable_errors(errors: &[Value]) -> (usize, BTreeMap<String, Vec<Value>>) {
    let mut snapshot_wide = 0usize;
    let mut by_run: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for error in errors {
        match run_id_key(error) {
            Some(run_id) => by_run.entry(run_id).or_default().push(error.clone()),
            None => snapshot_wide += 1,
        }
    }
    (snapshot_wide, by_run)
}

/// Separate the failures that can be filed from the ones whose evidence is
/// incomplete.
///
/// Per-finding evidence requirements are unchanged: a failure is filed only
/// when collection investigated it fully and no error is recorded against its
/// run. What changes is that a deferred failure now says so in its own entry
/// instead of silently withholding its neighbours.
fn split_deferred_failures(
    failures: &[Value],
    run_errors: &BTreeMap<String, Vec<Value>>,
) -> (Vec<Value>, Vec<Value>) {
    let mut complete = Vec::new();
    let mut deferred = Vec::new();
    for failure in failures {
        let reasons = run_id_key(failure)
            .and_then(|run_id| run_errors.get(&run_id).cloned())
            .unwrap_or_default();
        let investigated = failure.get("investigated").and_then(Value::as_bool) == Some(true);
        if reasons.is_empty() && investigated {
            complete.push(failure.clone());
            continue;
        }
        deferred.push(json!({
            "run_id": failure.get("run_id"),
            "url": failure.get("url"),
            "workflow": failure.get("workflow"),
            "head_branch": failure.get("head_branch"),
            "ref_kind": failure.get("ref_kind"),
            "investigated": investigated,
            "retryable": true,
            "reasons": if reasons.is_empty() {
                vec![json!({
                    "stage": "registration",
                    "operation": "current_failure_not_investigated",
                    "run_id": failure.get("run_id"),
                    "retryable": true,
                    "message": "a current CI failure has no complete investigation and cannot be filed safely",
                })]
            } else {
                reasons
            },
        }));
    }
    (complete, deferred)
}

/// The deferred entries flattened back into the error list shape, for the
/// ending where nothing could be filed at all.
fn deferred_errors(deferred: &[Value]) -> Vec<Value> {
    deferred
        .iter()
        .filter_map(|entry| entry.get("reasons").and_then(Value::as_array))
        .flat_map(|reasons| reasons.iter().cloned())
        .collect()
}

/// Make partial registration legible: an operator reading the audit must be
/// able to tell "three findings, three filed" from "three findings filed and
/// eleven still owed".
fn deferral_audit(mut audit: Value, deferred: &[Value]) -> Value {
    audit["deferred_failures"] = json!(deferred.len());
    audit["deferred_failure_run_ids"] = json!(
        deferred
            .iter()
            .filter_map(|entry| entry.get("run_id").cloned())
            .collect::<Vec<_>>()
    );
    audit["retryable_errors"] = json!(deferred_errors(deferred).len());
    audit
}

fn audit_summary(evidence: &Value, failures: &[Value]) -> Value {
    let latest_run_ids = evidence
        .get("latest_runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|run| run.get("run_id").cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_failure_run_ids = failures
        .iter()
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    let investigated_failure_run_ids = failures
        .iter()
        .filter(|run| run.get("investigated").and_then(Value::as_bool) == Some(true))
        .filter_map(|run| run.get("run_id").cloned())
        .collect::<Vec<_>>();
    json!({
        "latest_runs_discovered": latest_run_ids.len(),
        "latest_run_ids": latest_run_ids,
        "current_failures": current_failure_run_ids.len(),
        "current_failure_run_ids": current_failure_run_ids,
        "investigated_failures": investigated_failure_run_ids.len(),
        "investigated_failure_run_ids": investigated_failure_run_ids,
        "tasks_created": 0,
        "created_task_ids": [],
        "existing_task_skips": 0,
        "existing_task_owners": [],
        "deferred_failures": 0,
        "deferred_failure_run_ids": [],
        "retryable_errors": 0,
    })
}

fn filing_audit(mut audit: Value, filed: &[Value], skipped_existing: &[Value]) -> Value {
    let created_task_ids = filed
        .iter()
        .filter_map(|entry| entry.get("task_id").cloned())
        .collect::<Vec<_>>();
    let existing_task_owners = skipped_existing
        .iter()
        .filter_map(|entry| entry.get("task_id").cloned())
        .collect::<Vec<_>>();
    audit["tasks_created"] = json!(created_task_ids.len());
    audit["created_task_ids"] = json!(created_task_ids);
    audit["existing_task_skips"] = json!(skipped_existing.len());
    audit["existing_task_owners"] = json!(existing_task_owners);
    audit
}

fn retryable_pipeline_error(stage: &str, audit: &Value, errors: Vec<Value>) -> OrbitError {
    let mut audit = audit.clone();
    audit["retryable_errors"] = json!(errors.len());
    OrbitError::Execution(format!(
        "ci_failure_sweep retryable: {}",
        json!({
            "outcome": "retryable_error",
            "stage": stage,
            "audit": audit,
            "errors": errors,
        })
    ))
}

fn bounded_error(message: &str) -> String {
    redact_all(message).chars().take(500).collect()
}

fn normalize_retryable_error(error: Value) -> Value {
    json!({
        "stage": error.get("stage").and_then(Value::as_str).unwrap_or("collection"),
        "operation": error
            .get("operation")
            .or_else(|| error.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        "run_id": error.get("run_id").cloned().unwrap_or(Value::Null),
        "retryable": true,
        "message": bounded_error(
            error
                .get("message")
                .or_else(|| error.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("CI handoff operation failed"),
        ),
    })
}

/// One root cause, with every current run that exhibited it.
struct FailureCluster {
    /// Dedupe identity across sweeps: workflow, failing job, failing step, and
    /// normalized error signature. Commit-independent by design.
    failure_key: String,
    /// Grouping identity within one snapshot: `failure_key` plus the commit the
    /// runner actually tested.
    cluster_key: String,
    workflow: String,
    job: String,
    step: String,
    tested_commit: String,
    signature: String,
    /// True when `signature` is the failing step name because no error line
    /// survived in the excerpt. The description must label that as a fallback
    /// rather than a captured diagnostic; collapsing every distinct failure of
    /// the step into one `failure_key` is the weaker identity, not a quote.
    signature_is_step_fallback: bool,
    log_excerpt: String,
    log_truncated: bool,
    runs: Vec<Value>,
}

impl FailureCluster {
    fn duplicate_candidate(&self) -> DuplicateCandidate {
        let exact_tag = format!("{CI_FAILURE_KEY_TAG_PREFIX}{}", self.failure_key);
        let fingerprints = if self.signature_is_step_fallback {
            // A step-name fallback contains no diagnostic. It is sufficient
            // for exact-key idempotency but too weak for broader free-text
            // coverage, where it could suppress an unrelated failure of the
            // same generic CI step.
            vec![CoverageFingerprint::new(
                "ci_failure_unmatchable_fallback",
                vec![CoverageAnchor::new("exact_failure_key", &exact_tag)],
            )]
        } else {
            vec![CoverageFingerprint::new(
                "ci_failure_root_cause",
                vec![
                    CoverageAnchor::new("workflow", format!("workflow {}", self.workflow)),
                    CoverageAnchor::new("job", format!("failing job {}", self.job)),
                    CoverageAnchor::new("step", format!("failing step {}", self.step)),
                    CoverageAnchor::new("normalized_error_signature", &self.signature),
                ],
            )]
        };
        DuplicateCandidate::new(exact_tag, fingerprints)
    }

    fn run_urls(&self) -> Vec<String> {
        self.runs
            .iter()
            .filter_map(|run| run.get("url").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect()
    }

    fn run_ids(&self) -> Vec<Value> {
        self.runs
            .iter()
            .filter_map(|run| run.get("run_id").cloned())
            .collect()
    }

    fn filing_entry(&self, task_id: &str) -> Value {
        json!({
            "task_id": task_id,
            "failure_key": self.failure_key,
            "cluster_key": self.cluster_key,
            "workflow": self.workflow,
            "job": self.job,
            "step": self.step,
            "tested_commit": self.tested_commit,
            "run_urls": self.run_urls(),
        })
    }

    fn title(&self) -> String {
        let where_ = match (self.job.as_str(), self.step.as_str()) {
            ("", "") => self.workflow.clone(),
            (job, "") => format!("{} / {job}", self.workflow),
            ("", step) => format!("{} / {step}", self.workflow),
            (job, step) => format!("{} / {job} / {step}", self.workflow),
        };
        // Cap the body, not the whole string, so a long workflow/job/step
        // never eats into the prefix and the final title still respects the
        // existing 120-character bound.
        let body_budget = 120usize.saturating_sub(CI_FAILURE_SWEEP_TITLE_PREFIX.chars().count());
        let body = truncate_chars(&format!("Fix red CI: {where_}"), body_budget);
        format!("{CI_FAILURE_SWEEP_TITLE_PREFIX}{body}")
    }

    fn acceptance_criteria(&self) -> Vec<String> {
        vec![
            format!(
                "The root cause of the `{}` failure recorded below is fixed in this repository; \
                 the workflow, assertion, or lint level is not disabled, weakened, or made \
                 non-blocking to obtain green.",
                self.workflow
            ),
            "The exact command or narrowest faithful local equivalent that failed on the runner \
             is reproduced and then passes locally."
                .to_string(),
            "The repository's documented pre-handoff gate passes.".to_string(),
            "If the failure turns out to be infrastructure rather than repository-owned, the \
             execution summary cites concrete evidence for that (a same-commit retry that \
             succeeded, or runner/service fault output) rather than a single non-reproduction."
                .to_string(),
        ]
    }

    /// Render the evidence a remediation agent needs, inline.
    ///
    /// This is the whole point of the sweep: the agent that picks this task up
    /// cannot reach GitHub, so anything absent here is unavailable to it.
    fn description(&self, evidence: &Value) -> String {
        let mut out = String::new();
        out.push_str(
            "This task was filed automatically from a host-side sweep of this repository's \
             GitHub Actions runs. Every CI query ran on the host before this task existed, so \
             the evidence below is all of it — the execution lane for this task cannot reach \
             GitHub, and it is not expected to.\n\n",
        );

        out.push_str("## Failure\n\n");
        out.push_str(&format!("- Workflow: `{}`\n", display(&self.workflow)));
        out.push_str(&format!("- Failing job: `{}`\n", display(&self.job)));
        out.push_str(&format!("- Failing step: `{}`\n", display(&self.step)));
        out.push_str(&format!(
            "- Commit the runner actually checked out: `{}`\n",
            display(&self.tested_commit)
        ));
        if self.signature_is_step_fallback {
            out.push_str(&format!(
                "- Normalized error signature (step-name fallback — no error line was captured; \
                 the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        } else {
            out.push_str(&format!(
                "- Normalized error signature (the dedupe identity, not a quote): `{}`\n",
                display(&self.signature)
            ));
        }
        if let Some(repository) = evidence.get("repository").and_then(Value::as_object) {
            if let Some(full_name) = repository.get("full_name").and_then(Value::as_str) {
                out.push_str(&format!("- Repository: `{full_name}`\n"));
            }
            if let Some(default_branch) = repository.get("default_branch").and_then(Value::as_str) {
                out.push_str(&format!(
                    "- Release branch as GitHub reports it: `{default_branch}`\n"
                ));
            }
        }
        if let Some(collected_at) = evidence.get("collected_at").and_then(Value::as_str) {
            out.push_str(&format!("- Evidence collected at: {collected_at}\n"));
        }

        out.push_str("\n## Runs exhibiting this failure\n\n");
        out.push_str(
            "Three commits are routinely conflated and are kept apart here: the SHA the \
             workflow event reported, the SHA the ref points at now, and the commit the runner \
             actually checked out. A pull-request merge SHA is not a pull-request head SHA.\n\n",
        );
        for run in self.runs.iter().take(MAX_LISTED_RUNS) {
            out.push_str(&render_run(run));
        }
        if self.runs.len() > MAX_LISTED_RUNS {
            out.push_str(&format!(
                "\n_{} further run(s) in this cluster are not listed._\n",
                self.runs.len() - MAX_LISTED_RUNS
            ));
        }

        out.push_str("\n## Failed-step log excerpt\n\n");
        if self.log_excerpt.trim().is_empty() {
            out.push_str(
                "_No log excerpt was captured for this cluster. Reproduce the failing job's \
                 command locally instead of guessing from the step name._\n",
            );
            let relevant = relevant_log_query_errors(evidence, &self.runs);
            if !relevant.is_empty() {
                out.push('\n');
                for error in relevant {
                    out.push_str(&format!(
                        "- `{}` for run `{}`: {}\n",
                        display(&value_string(error, "query")),
                        display(&value_string(error, "run_id")),
                        truncate_chars(&value_string(error, "error"), 300),
                    ));
                }
            }
        } else {
            let excerpt = render_failed_step_excerpt(&self.log_excerpt, DESCRIPTION_LOG_BYTES);
            if !excerpt.body.trim().is_empty() {
                out.push_str("```\n");
                out.push_str(&excerpt.body);
                out.push_str("\n```\n");
            }
            if !excerpt.has_anchor {
                out.push_str(
                    "\n_No error anchor was present in the retained excerpt; the env/with dump \
                     is omitted rather than shown as evidence._\n",
                );
            }
            if self.log_truncated {
                out.push_str(
                    "\n_The excerpt above was truncated at collection. Head and tail are kept; \
                     the omitted region is marked inline._\n",
                );
            }
        }

        let stale = self.stale_evidence(evidence);
        if !stale.is_empty() {
            out.push_str("\n## Stale or superseded runs of this workflow\n\n");
            out.push_str(
                "These are already excluded from the failure above. They are listed so the \
                 repair is not attributed to a run that no longer reflects the current head.\n\n",
            );
            for entry in &stale {
                out.push_str(&format!(
                    "- `{}` run {} — {}: {}\n",
                    display(&value_string(entry, "workflow")),
                    display(&value_string(entry, "url")),
                    display(&value_string(entry, "reason")),
                    display(&value_string(entry, "evidence")),
                ));
            }
        }

        if let Some(truncation) = evidence.get("truncation") {
            out.push_str("\n## Collection bounds\n\n");
            out.push_str(
                "Reported so \"no more failures\" is never mistaken for \"we stopped \
                 looking\".\n\n",
            );
            out.push_str("```json\n");
            out.push_str(&truncate_bytes(
                &serde_json::to_string_pretty(truncation).unwrap_or_default(),
                2_000,
            ));
            out.push_str("\n```\n");
        }

        let query_errors = evidence
            .get("query_errors")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if !query_errors.is_empty() {
            out.push_str("\n## Queries that failed during collection\n\n");
            for error in query_errors {
                out.push_str(&format!("- {}\n", truncate_chars(&error.to_string(), 300)));
            }
        }

        out.push_str(
            "\n## How to finish\n\n\
             Reproduce the failure at the current repository head using the exact command or \
             the narrowest faithful local equivalent, fix the repository-owned cause, and rerun \
             that command. Do not disable a workflow, weaken an assertion or lint level, add a \
             broad allow rule, or mark a failing gate non-blocking. Then run the repository's \
             documented pre-handoff gate. Verification happens on this task's own pull request: \
             CI runs there normally, and if the failure is still current the next sweep will see \
             it again.\n",
        );
        out
    }

    /// Stale/superseded runs of the same workflow, so the description can say
    /// why they were excluded rather than leaving them unexplained.
    fn stale_evidence(&self, evidence: &Value) -> Vec<Value> {
        evidence
            .get("stale_or_superseded")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| value_string(entry, "workflow") == self.workflow)
                    .take(MAX_LISTED_RUNS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn render_run(run: &Value) -> String {
    let mut out = format!(
        "- {} run `{}` ({} on `{}`) — status `{}`, conclusion `{}`\n",
        display(&value_string(run, "url")),
        display(&value_string(run, "run_id")),
        display(&value_string(run, "event")),
        display(&value_string(run, "head_branch")),
        display(&value_string(run, "status")),
        display(&value_string(run, "conclusion")),
    );
    out.push_str(&format!(
        "  - event-reported head SHA: `{}`\n",
        display(&value_string(run, "event_reported_head_sha"))
    ));
    out.push_str(&format!(
        "  - current head of that ref: `{}`\n",
        display(&value_string(run, "current_ref_head_sha"))
    ));
    let checkout = run
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .map(|shas| {
            shas.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "  - commit actually checked out: `{}`\n",
        display(&checkout)
    ));
    if let Some(pr) = run.get("pr_number").and_then(Value::as_u64) {
        out.push_str(&format!("  - pull request: #{pr}\n"));
    }
    for line in run
        .get("checkout_evidence")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .take(3)
    {
        out.push_str(&format!(
            "  - checkout evidence: `{}`\n",
            truncate_chars(line, 200)
        ));
    }
    for job in run
        .get("failed_jobs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(3)
    {
        out.push_str(&format!(
            "  - failed job `{}` (id `{}`): {}\n",
            display(&value_string(job, "name")),
            display(&value_string(job, "job_id")),
            display(&value_string(job, "url")),
        ));
        let steps = job
            .get("failed_steps")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|step| display(&value_string(step, "name")).to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        if !steps.is_empty() {
            out.push_str(&format!("  - failing step(s): `{steps}`\n"));
        }
    }
    out
}

/// Group current failures by root cause.
///
/// Preserves the order collection chose (integration head first, then release,
/// then pull requests), so the filing cap spends itself on the heads that gate
/// delivery.
fn cluster_failures(failures: &[Value]) -> Vec<FailureCluster> {
    let mut order: Vec<String> = Vec::new();
    let mut grouped: BTreeMap<String, FailureCluster> = BTreeMap::new();

    for failure in failures {
        // A listed-but-uninvestigated failure carries no job, step, or log —
        // only a run URL. Filing a task from it would produce a task whose
        // evidence section is empty, which is worse than reporting it as an
        // unfiled bound. `truncation` already names how many there were.
        if failure.get("investigated").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let workflow = value_string(failure, "workflow");
        let (job, step) = failing_job_and_step(failure);
        let log_excerpt = value_string(failure, "log_excerpt");
        let signature = error_signature(&log_excerpt, &step);
        let tested_commit = tested_commit(failure);

        let failure_key = digest(&[&workflow, &job, &step, &signature.text]);
        let cluster_key = digest(&[&workflow, &job, &step, &signature.text, &tested_commit]);

        let cluster = grouped.entry(cluster_key.clone()).or_insert_with(|| {
            order.push(cluster_key.clone());
            FailureCluster {
                failure_key,
                cluster_key: cluster_key.clone(),
                workflow,
                job,
                step,
                tested_commit,
                signature: signature.text,
                signature_is_step_fallback: signature.step_fallback,
                log_excerpt,
                log_truncated: failure
                    .get("log_truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                runs: Vec::new(),
            }
        });
        cluster.runs.push(failure.clone());
    }

    order
        .into_iter()
        .filter_map(|key| grouped.remove(&key))
        .collect()
}

/// The first failing job and step named by the snapshot.
fn failing_job_and_step(failure: &Value) -> (String, String) {
    let Some(job) = failure
        .get("failed_jobs")
        .and_then(Value::as_array)
        .and_then(|jobs| jobs.first())
    else {
        return (String::new(), String::new());
    };
    let step = job
        .get("failed_steps")
        .and_then(Value::as_array)
        .and_then(|steps| steps.first())
        .map(|step| value_string(step, "name"))
        .unwrap_or_default();
    (value_string(job, "name"), step)
}

/// The commit under test: what the runner checked out when that is evidenced,
/// falling back to the SHA the event reported.
fn tested_commit(failure: &Value) -> String {
    failure
        .get("actual_checkout_shas")
        .and_then(Value::as_array)
        .and_then(|shas| shas.first())
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value_string(failure, "event_reported_head_sha"))
}

/// Lines a runner emits when something breaks.
const ERROR_MARKERS: &[&str] = &[
    "error",
    "failed",
    "failure",
    "panicked",
    "assertion",
    "exit code",
    "not ok",
    "fatal",
];

/// Reduce a failed-step log to one normalized line that survives a rerun.
///
/// Reruns of the same regression differ in timestamps, durations, run numbers,
/// and paths under a run-specific temp directory. Normalizing those away is
/// what lets an hourly sweep recognize the same root cause instead of filing it
/// again every hour. Prefer an `##[error]`-annotated line over an unanchored
/// marker substring, except that GitHub's generic runner-completion annotation
/// yields to a specific unannotated diagnostic immediately before it. Never
/// sign off runner-bookkeeping (checkout, group headers, `env:`/`with:` dumps).
/// With no usable line the step name alone is the signature — weaker, but
/// stable, and still scoped by workflow and job.
fn error_signature(log_excerpt: &str, step: &str) -> ErrorSignature {
    let lines = classify_log_lines(log_excerpt);
    for (kind, line) in &lines {
        if *kind == LineKind::ErrorAnnotated {
            return ErrorSignature {
                text: normalize_signature(&log_payload(line).to_ascii_lowercase()),
                step_fallback: false,
            };
        }
    }
    for (kind, line) in &lines {
        if *kind == LineKind::Marker {
            return ErrorSignature {
                text: normalize_signature(&log_payload(line).to_ascii_lowercase()),
                step_fallback: false,
            };
        }
    }
    for (index, (kind, _)) in lines.iter().enumerate() {
        if *kind != LineKind::RunnerCompletion {
            continue;
        }
        if let Some((_, line)) = lines[..index]
            .iter()
            .rev()
            .find(|(kind, line)| *kind == LineKind::Content && is_diagnostic_content(line))
        {
            return ErrorSignature {
                text: normalize_signature(&log_payload(line).to_ascii_lowercase()),
                step_fallback: false,
            };
        }
    }
    for (kind, line) in &lines {
        if *kind == LineKind::RunnerCompletion {
            return ErrorSignature {
                text: normalize_signature(&log_payload(line).to_ascii_lowercase()),
                step_fallback: false,
            };
        }
    }
    ErrorSignature {
        text: normalize_signature(&step.to_ascii_lowercase()),
        step_fallback: true,
    }
}

struct ErrorSignature {
    text: String,
    step_fallback: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    RunCommand,
    ParamDump,
    EndGroup,
    ErrorAnnotated,
    RunnerCompletion,
    Marker,
    Bookkeeping,
    Content,
}

impl LineKind {
    fn skip_from_excerpt(self) -> bool {
        matches!(self, Self::ParamDump | Self::EndGroup | Self::Bookkeeping)
    }
}

struct FailedStepExcerpt {
    body: String,
    has_anchor: bool,
}

/// Command line plus the failure region, never a head-biased env dump.
///
/// The `##[group]Run …` line is the command the runner executed and is the
/// most actionable fact in the log. The `env:` / `with:` dump that follows it
/// is never the evidence. The rest of the block is a bounded window around
/// an error anchor, capped at `max_bytes` on that region rather than the
/// log head.
fn render_failed_step_excerpt(log: &str, max_bytes: usize) -> FailedStepExcerpt {
    let lines = classify_log_lines(log);
    let command = lines
        .iter()
        .find(|(kind, _)| *kind == LineKind::RunCommand)
        .map(|(_, line)| *line);

    let anchor = lines
        .iter()
        .position(|(kind, _)| matches!(kind, LineKind::ErrorAnnotated | LineKind::RunnerCompletion))
        .or_else(|| lines.iter().position(|(kind, _)| *kind == LineKind::Marker));

    let Some(anchor_idx) = anchor else {
        return FailedStepExcerpt {
            body: command.unwrap_or("").to_string(),
            has_anchor: false,
        };
    };

    const LINES_BEFORE: usize = 24;
    const LINES_AFTER: usize = 12;
    let kept: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| !kind.skip_from_excerpt())
        .map(|(idx, _)| idx)
        .collect();
    let anchor_in_kept = kept.iter().position(|idx| *idx == anchor_idx).unwrap_or(0);
    let start = anchor_in_kept.saturating_sub(LINES_BEFORE);
    let end = (anchor_in_kept + 1 + LINES_AFTER).min(kept.len());
    let mut window: Vec<&str> = kept[start..end].iter().map(|idx| lines[*idx].1).collect();
    if let Some(command) = command
        && !window.contains(&command)
    {
        window.insert(0, command);
    }
    let joined = window.join("\n");
    FailedStepExcerpt {
        body: cap_bytes_around_line(&joined, lines[anchor_idx].1, max_bytes),
        has_anchor: true,
    }
}

fn classify_log_lines(log: &str) -> Vec<(LineKind, &str)> {
    let mut in_param_block = false;
    let mut out = Vec::new();
    for line in log.lines() {
        let payload = log_payload(line);
        let trimmed = payload.trim();
        let lowered = trimmed.to_ascii_lowercase();
        let indented = payload.starts_with(' ') || payload.starts_with('\t');
        let kind = if is_run_command_payload(trimmed) {
            in_param_block = false;
            LineKind::RunCommand
        } else if lowered.contains("##[endgroup]") {
            in_param_block = false;
            LineKind::EndGroup
        } else if lowered == "env:" || lowered == "with:" {
            in_param_block = true;
            LineKind::ParamDump
        } else if in_param_block && (indented || trimmed.is_empty()) {
            LineKind::ParamDump
        } else {
            in_param_block = false;
            if is_generic_runner_completion(&lowered) {
                LineKind::RunnerCompletion
            } else if lowered.contains("##[error]") {
                LineKind::ErrorAnnotated
            } else if is_runner_bookkeeping(&lowered) || lowered.contains("##[group]") {
                LineKind::Bookkeeping
            } else if ERROR_MARKERS.iter().any(|marker| lowered.contains(marker)) {
                LineKind::Marker
            } else {
                LineKind::Content
            }
        };
        out.push((kind, line));
    }
    out
}

fn is_generic_runner_completion(lowered: &str) -> bool {
    let Some(message) = lowered.trim().strip_prefix("##[error]") else {
        return false;
    };
    let Some(exit_code) = message
        .trim()
        .strip_prefix("process completed with exit code ")
    else {
        return false;
    };
    let exit_code = exit_code.trim_end_matches('.');
    !exit_code.is_empty() && exit_code.chars().all(|ch| ch.is_ascii_digit())
}

fn is_diagnostic_content(line: &str) -> bool {
    let payload = log_payload(line).trim();
    !payload.is_empty() && !payload.starts_with("##[")
}

fn is_run_command_payload(payload: &str) -> bool {
    let lowered = payload.trim().to_ascii_lowercase();
    lowered.starts_with("##[group]run ") || lowered == "##[group]run"
}

fn is_runner_bookkeeping(lowered: &str) -> bool {
    lowered.starts_with("head is now at") || lowered.starts_with("syncing repository")
}

/// Cap `text` at `max_bytes` while keeping `anchor_line`, not the head.
fn cap_bytes_around_line(text: &str, anchor_line: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let Some(anchor_start) = text.find(anchor_line) else {
        return truncate_bytes(text, max_bytes);
    };
    let anchor_len = anchor_line.len();
    if anchor_len >= max_bytes {
        return truncate_bytes(anchor_line, max_bytes);
    }
    let extra = max_bytes - anchor_len;
    let want_before = extra * 2 / 3;
    let mut start = anchor_start.saturating_sub(want_before);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    if start > 0
        && let Some(newline) = text[start..anchor_start].find('\n')
    {
        start += newline + 1;
    }
    let mut end = start.saturating_add(max_bytes).min(text.len());
    if end < anchor_start + anchor_len {
        end = (anchor_start + anchor_len).min(text.len());
        start = end.saturating_sub(max_bytes);
        while start > 0 && !text.is_char_boundary(start) {
            start -= 1;
        }
    }
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    if end < text.len()
        && let Some(newline) = text[anchor_start + anchor_len..end].rfind('\n')
    {
        end = anchor_start + anchor_len + newline;
    }
    let mut out = String::new();
    if start > 0 {
        out.push_str("[...]\n");
    }
    out.push_str(&text[start..end]);
    if end < text.len() {
        out.push_str(&format!(
            "\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
             the sweep run's step output ...]"
        ));
    }
    out
}

/// `query_errors` entries for this cluster's failed-step log fetch, if any.
fn relevant_log_query_errors<'a>(evidence: &'a Value, runs: &[Value]) -> Vec<&'a Value> {
    let run_ids: BTreeSet<String> = runs
        .iter()
        .map(|run| value_string(run, "run_id"))
        .filter(|id| !id.is_empty())
        .collect();
    evidence
        .get("query_errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter(|error| {
                    let query = value_string(error, "query");
                    let run_id = value_string(error, "run_id");
                    matches!(query.as_str(), "run_logs" | "run_logs_all")
                        && run_ids.contains(&run_id)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Strip the `job<TAB>step<TAB>timestamp ` columns a runner log carries.
fn log_payload(line: &str) -> &str {
    let mut columns = line.splitn(3, '\t');
    let rest = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(_step), Some(rest)) => rest,
        _ => line,
    };
    match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    }
}

/// Collapse the parts of a log line that vary between identical failures.
fn normalize_signature(lowered: &str) -> String {
    let mut out = String::with_capacity(lowered.len());
    let mut chars = lowered.chars().peekable();
    let mut last_was_space = false;
    while let Some(ch) = chars.next() {
        if ch.is_ascii_alphanumeric() {
            let mut token = String::from(ch);
            while chars.peek().is_some_and(char::is_ascii_alphanumeric) {
                token.push(chars.next().unwrap_or_default());
            }
            let replacement = if token.chars().all(|c| c.is_ascii_digit()) {
                "<n>"
            } else if token.len() >= 7 && token.chars().all(|c| c.is_ascii_hexdigit()) {
                "<hex>"
            } else {
                token.as_str()
            };
            out.push_str(replacement);
            last_was_space = false;
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        out.push(ch);
        last_was_space = false;
    }
    truncate_chars(out.trim(), 200)
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
        .chars()
        .take(KEY_LEN)
        .collect()
}

fn bounded_u64(input: &Value, key: &str, default: u64, max: u64) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("input.{key} must be a positive integer")))?;
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "input.{key} must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

/// Read a snapshot field as a display string, accepting the numeric spellings
/// `gh` uses for run and job identifiers.
fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "…"
}

fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
         the sweep run's step output ...]",
        &value[..end]
    )
}
