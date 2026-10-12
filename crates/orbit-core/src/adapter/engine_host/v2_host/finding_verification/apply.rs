//! Validate verifier verdicts, escalate the doubtful ones once, and record
//! the deciding verdict on each task.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::workflow::automation::members::MemberEvidence;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{
    FindingVerification, VerdictWrite, VerifierReport, VerifierVerdict, next_tier,
};

use super::super::task_pilot::input::{
    requested_workspace_root, required_string, required_string_array,
};
use super::super::task_pilot::source::SourceSnapshot;
use super::prepare::task_snapshot;

const MAX_PARTITION_SIZE: usize = 5;

/// One prepared task, as apply trusts it.
struct Prepared {
    status: TaskStatus,
    history_len: usize,
    finding_fingerprint: Option<String>,
    crew: String,
    tier: TaskComplexity,
    prior: Vec<VerifierVerdict>,
}

pub(in super::super) fn apply(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let prepared = input
        .get("prepared")
        .filter(|prepared| prepared.is_object())
        .ok_or_else(|| DispatchError::action_failed(action, "`prepared` must be an object"))?;
    let prepared_workspace = required_string(prepared, "workspace_path", action)?;
    if Path::new(prepared_workspace) != workspace_root {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "prepared workspace {prepared_workspace} does not match active workspace {}",
                workspace_root.display()
            ),
        ));
    }
    let source = SourceSnapshot::from_prepared(prepared, action)?.ok_or_else(|| {
        DispatchError::action_failed(action, "prepared.source must pin a revision")
    })?;
    source.ensure_commit(action, &workspace_root)?;
    let claim =
        crate::application::automation::members::claim(runtime, prepared).map_err(failed)?;
    let escalation_round = prepared.get("round").and_then(Value::as_str) == Some("escalation");
    let partitions = prepared
        .get("partitions")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DispatchError::action_failed(action, "prepared.partitions must be an array")
        })?;
    let results = input
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| DispatchError::action_failed(action, "`results` must be an array"))?;
    let snapshots = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| DispatchError::action_failed(action, "prepared.tasks must be an array"))?
        .iter()
        .map(|entry| {
            Ok((
                required_string(entry, "task_id", action)?.to_string(),
                Prepared {
                    status: prepared_field(entry, "status", action)?,
                    history_len: prepared_field(entry, "history_len", action)?,
                    finding_fingerprint: entry
                        .get("finding_fingerprint")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    crew: required_string(entry, "crew", action)?.to_string(),
                    tier: prepared_field(entry, "tier", action)?,
                    prior: prepared_field(entry, "prior", action)?,
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, DispatchError>>()?;

    let mut task_outcomes = input
        .get("carried_task_outcomes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    task_outcomes.extend(
        prepared
            .get("superseded_during_preparation")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
    );
    let mut recorded = BTreeMap::<String, Value>::new();
    let mut escalations = BTreeMap::<String, Vec<Value>>::new();

    for (position, partition) in partitions.iter().enumerate() {
        let task_ids = required_string_array(partition, "task_ids", action)?;
        let crew = required_string(partition, "crew", action)?;
        // The host matches results by position; a verifier that failed
        // returns no object, and every task it held stays unverified.
        let reports = results
            .get(position)
            .and_then(|result| result.get("tasks"))
            .and_then(Value::as_array);
        for task_id in &task_ids {
            let snapshot = snapshots.get(task_id).ok_or_else(|| {
                DispatchError::action_failed(
                    action,
                    format!("task {task_id} was not in prepared.tasks"),
                )
            })?;
            if snapshot.crew != crew {
                return Err(DispatchError::action_failed(
                    action,
                    format!(
                        "task {task_id} was prepared for crew {}, not {crew}",
                        snapshot.crew
                    ),
                ));
            }
            let Some(report) = reports.and_then(|reports| {
                reports
                    .iter()
                    .find(|report| report.get("task_id").and_then(Value::as_str) == Some(task_id))
            }) else {
                let error = input
                    .get("partition_errors")
                    .and_then(Value::as_array)
                    .and_then(|errors| errors.get(position))
                    .and_then(Value::as_str);
                task_outcomes.push(match error {
                    Some(error) => unverified(
                        task_id,
                        "verifier_failed",
                        format!("verifier partition {position} (crew {crew}) failed: {error}"),
                    ),
                    None => unverified(
                        task_id,
                        "missing_verdict",
                        format!("verifier crew {crew} returned no verdict for this task"),
                    ),
                });
                continue;
            };
            let report = match serde_json::from_value::<VerifierReport>(report.clone())
                .map_err(|error| error.to_string())
                .and_then(|report| {
                    report.validate()?;
                    Ok(report)
                }) {
                Ok(report) => report,
                Err(error) => {
                    task_outcomes.push(unverified(task_id, "invalid_verdict", error));
                    continue;
                }
            };
            if let Err(error) = check_evidence(action, &source, &workspace_root, &report)? {
                task_outcomes.push(unverified(task_id, "invalid_evidence", error));
                continue;
            }

            let mut crews = snapshot.prior.clone();
            crews.push(VerifierVerdict {
                crew: crew.to_string(),
                tier: snapshot.tier,
                verdict: report.verdict,
                confidence: report.confidence,
            });
            // A doubtful first verdict is judged again by the next tier's
            // pool before anything is recorded; the second verdict decides.
            if !escalation_round
                && report.escalates_from(snapshot.tier)
                && let Some(next) = next_tier(snapshot.tier)
            {
                let task = runtime.get_task(task_id).map_err(failed)?;
                let avoid = crews
                    .iter()
                    .map(|verdict| verdict.crew.as_str())
                    .chain(task.created_by.as_deref())
                    .collect::<Vec<_>>();
                if let Some(verifier) = runtime
                    .draw_finding_verifier(&task, next, &avoid)
                    .map_err(failed)?
                {
                    let prior = crews
                        .iter()
                        .map(|verdict| json!(verdict))
                        .collect::<Vec<_>>();
                    let mut entry = task_snapshot(&task, snapshot.history_len, &verifier, &prior);
                    entry["status"] = json!(snapshot.status);
                    // The second verifier decides the same finding as the
                    // first, even if this re-read sees edited task material.
                    entry["finding_fingerprint"] = json!(snapshot.finding_fingerprint);
                    escalations
                        .entry(verifier.crew.clone())
                        .or_default()
                        .push(entry);
                    continue;
                }
            }

            let record = FindingVerification::decided(
                &report,
                crews,
                &source.source_revision,
                snapshot.finding_fingerprint.as_deref(),
                &source.base_branch,
                run_id,
            );
            match runtime
                .record_finding_verification(
                    task_id,
                    &record,
                    snapshot.status,
                    snapshot.history_len,
                )
                .map_err(failed)?
            {
                VerdictWrite::Recorded(status) => {
                    let record = json!(record);
                    task_outcomes.push(json!({
                        "task_id": task_id,
                        "outcome": "verified",
                        "verdict": record["verdict"],
                        "confidence": record["confidence"],
                        "crews": record["crews"],
                        "status": status,
                    }));
                    recorded.insert(task_id.clone(), record);
                }
                VerdictWrite::Superseded { reason, detail } => task_outcomes.push(json!({
                    "task_id": task_id, "outcome": "superseded", "reason": reason,
                    "detail": detail,
                })),
            }
        }
    }

    let escalation_tasks = escalations.values().flatten().cloned().collect::<Vec<_>>();
    let escalation_partitions = escalations
        .iter()
        .flat_map(|(crew, entries)| {
            entries
                .chunks(MAX_PARTITION_SIZE)
                .map(move |chunk| (crew.clone(), chunk))
        })
        .enumerate()
        .map(|(partition_index, (crew, entries))| {
            json!({
                "partition_index": partition_index,
                "task_ids": entries.iter().map(|entry| &entry["task_id"]).collect::<Vec<_>>(),
                "crew": crew,
            })
        })
        .collect::<Vec<_>>();
    let mut escalation_prepared = prepared.clone();
    escalation_prepared["round"] = json!("escalation");
    escalation_prepared["task_ids"] = json!(
        escalation_tasks
            .iter()
            .map(|entry| &entry["task_id"])
            .collect::<Vec<_>>()
    );
    escalation_prepared["task_count"] = json!(escalation_tasks.len());
    escalation_prepared["tasks"] = json!(escalation_tasks);
    escalation_prepared["partition_count"] = json!(escalation_partitions.len());
    escalation_prepared["partitions"] = json!(escalation_partitions);
    escalation_prepared["superseded_during_preparation"] = json!([]);

    let unverified_count = task_outcomes
        .iter()
        .filter(|outcome| outcome["outcome"] == "unverified")
        .count();
    let status = if unverified_count == 0 {
        "succeeded"
    } else {
        "failed"
    };
    let member_evidence = claim.as_ref().map(|claim| {
        claim
            .members()
            .iter()
            .filter_map(|member| {
                let result = member.task_ids.first().and_then(|id| recorded.get(id))?;
                Some(MemberEvidence {
                    action_id: claim.action_id.clone().unwrap_or_default(),
                    attempt_id: claim.id.clone(),
                    member_key: member.key.clone(),
                    input_fingerprint: member.fingerprint.clone(),
                    // A recorded verdict takes the task out of observation,
                    // so the fingerprint it was claimed at is its last.
                    resulting_fingerprint: member.fingerprint.clone(),
                    ready: true,
                    result: result.clone(),
                })
            })
            .collect::<Vec<_>>()
    });

    Ok(json!({
        "status": status,
        "error": (unverified_count != 0).then(|| {
            format!("{unverified_count} finding(s) left unverified; see task_outcomes")
        }),
        "mode": prepared.get("mode").cloned().unwrap_or(Value::Null),
        "round": if escalation_round { "escalation" } else { "initial" },
        "workspace_path": workspace_root,
        "source": source.to_json(),
        "partition_count": partitions.len(),
        "received_partition_count": results.len(),
        "task_outcomes": task_outcomes,
        "recorded_count": recorded.len(),
        "unverified_count": unverified_count,
        "escalation_count": escalation_partitions.len(),
        "escalation_prepared": escalation_prepared,
        "member_evidence": member_evidence,
    }))
}

/// Whether every location `report` cites names a line of a file that exists
/// at the pinned revision. `Ok(Err(..))` is a verdict defect, not a failure
/// of the action.
fn check_evidence(
    action: &str,
    source: &SourceSnapshot,
    workspace: &Path,
    report: &VerifierReport,
) -> Result<Result<(), String>, DispatchError> {
    for evidence in &report.evidence {
        let (path, line) = match evidence.path_and_line() {
            Ok(location) => location,
            Err(error) => return Ok(Err(error)),
        };
        match source.file_line_count(action, workspace, path)? {
            None => {
                return Ok(Err(format!(
                    "evidence cites {path}, which is not a file at {}",
                    source.source_revision
                )));
            }
            Some(lines) if usize::try_from(line).is_ok_and(|line| line > lines) => {
                return Ok(Err(format!(
                    "evidence cites {path}:{line}, past the {lines} lines it has at {}",
                    source.source_revision
                )));
            }
            Some(_) => {}
        }
    }
    Ok(Ok(()))
}

fn prepared_field<T: serde::de::DeserializeOwned>(
    entry: &Value,
    field: &str,
    action: &str,
) -> Result<T, DispatchError> {
    serde_json::from_value(entry.get(field).cloned().unwrap_or(Value::Null)).map_err(|error| {
        DispatchError::action_failed(action, format!("prepared task {field}: {error}"))
    })
}

fn unverified(task_id: &str, reason: &str, detail: String) -> Value {
    json!({"task_id": task_id, "outcome": "unverified", "reason": reason, "detail": detail})
}
