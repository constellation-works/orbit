//! Exact publication predicates and bounded evidence for refused provider reads.
use std::{path::Path, time::Duration};

use chrono::{DateTime, Utc};
use orbit_common::{
    OrbitError, security::redaction::redact_all, security::release::sha256_hex,
    text::floor_char_boundary,
};
use orbit_types::{
    task::{
        PublicationProofValue, PublicationPushCheckpoint, PublicationPushDecision,
        PublicationPushProof, PublicationSettlingDecision, PublicationSettlingObservation,
        TaskDecision, TaskHistoryEntry,
    },
    workflow::{JobRunState, SourceRepairBinding},
};
use serde_json::{Value, json};

use super::super::{
    candidate_resume::source_repair::publication_admission,
    git::git_output,
    handoff::HandoffContext,
    operations,
    remote_probe::{fixed_wait, remote_ref_output},
};
use super::{PullRequestStatus, read_pr_status};
use crate::context::{RuntimeHost, TaskAutomationUpdate};

/// Compare before projecting/redacting evidence. Missing GraphQL merge metadata
/// cannot establish absence of a merge; REST provisional merge SHAs are ignored.
pub(in crate::executor::automation::vcs) fn mismatches(
    binding: &SourceRepairBinding,
    pr: &PullRequestStatus,
    expected_head: &str,
) -> Vec<Value> {
    let raw = pr.provider_state();
    if !pr.identity_metadata_readable {
        return vec![
            json!({"field":"observation", "expected":"pull_request object", "observed":{"presence":"invalid", "value":bounded_value(raw)}}),
        ];
    }
    let checks = [
        (
            "number",
            binding
                .pull_request
                .parse::<u64>()
                .map_or_else(|_| json!(binding.pull_request), |number| json!(number)),
            pr.number.map(|n| n.to_string()).as_deref() == Some(binding.pull_request.as_str()),
        ),
        (
            "url",
            json!(binding.pull_request_url),
            pr.url.as_deref() == Some(binding.pull_request_url.as_str()),
        ),
        ("state", json!("OPEN"), pr.state.as_deref() == Some("OPEN")),
        (
            "headRefName",
            json!(binding.branch),
            pr.head_branch.as_deref() == Some(binding.branch.as_str()),
        ),
        (
            "headRefOid",
            json!(expected_head),
            pr.head_sha.as_deref() == Some(expected_head),
        ),
        (
            "baseRefName",
            json!(binding.base_branch),
            pr.base_branch.as_deref() == Some(binding.base_branch.as_str()),
        ),
        (
            "autoMergeRequest",
            Value::Null,
            pr.auto_merge_enabled == Some(false),
        ),
        (
            "mergeCommit",
            Value::Null,
            pr.merge_commit_absent && pr.merge_commit.is_none(),
        ),
        (
            "mergedAt",
            Value::Null,
            pr.merged_at_absent && pr.merged_at.is_none(),
        ),
    ];
    checks.into_iter().filter(|(_, _, matches)| !matches).map(|(field, expected, _)| {
        json!({"field":field, "expected":bounded_value(&expected), "observed":observed(raw.get(field))})
    }).collect()
}

const OBSERVED: &str = "source_repair_publication_observed";
const REFUSED: &str = "source_repair_publication_refused";
const MAX_REREADS: u64 = 3;
const POLL_SECONDS: u64 = 5;

/// Append-only reservations survive action retries and interruption between
/// recording a wait and performing it. No successful pipeline receipt is edited.
#[derive(Default)]
struct Settling {
    rereads: u64,
    waited_seconds: u64,
    settled_heads: Vec<String>,
    pending_push: Option<(String, Option<PublicationPushProof>)>,
}

impl Settling {
    fn load<H: RuntimeHost + ?Sized>(host: &H, task: &str, run: &str) -> Result<Self, OrbitError> {
        let mut budget = Self::default();
        for entry in host.get_task_history(task)? {
            if entry.by != "system" || entry.event != OBSERVED {
                continue;
            }
            let Some(observation) = PublicationSettlingObservation::from_entry(&entry, run)
                .map_err(|error| OrbitError::Execution(error.to_string()))?
            else {
                continue;
            };
            let delay = observation.reserved_delay_seconds;
            if delay > 0 {
                budget.rereads = budget.rereads.saturating_add(1);
                budget.waited_seconds = budget.waited_seconds.saturating_add(delay);
                let head = observation.published_head.as_text().ok_or_else(|| {
                    OrbitError::Execution("publication settling head unreadable".into())
                })?;
                budget.pending_push = Some((head.into(), observation.successful_push));
            }
            if observation.decision == PublicationSettlingDecision::Settled
                && let Some(head) = observation.published_head.as_text()
            {
                budget.settled_heads.push(head.into());
                budget.pending_push = None;
            }
        }
        Ok(budget)
    }
}

/// Only authenticated replacement metadata can settle. Every observation,
/// including convergence, reapplies authority, identity and merge predicates.
pub(super) fn verify_publication<H: RuntimeHost + ?Sized>(
    host: &H,
    context: &HandoffContext,
    binding: &SourceRepairBinding,
    head: &str,
    base: &str,
) -> Result<(), OrbitError> {
    let mut budget = Settling::load(host, &binding.task_id, &context.batch_id)?;
    let admission = admission_evidence(binding)?;
    let binding_hash =
        sha256_hex(&serde_json::to_vec(binding).map_err(|e| OrbitError::Execution(e.to_string()))?);
    loop {
        let started_at = Utc::now();
        let read = read_pr_status(host, &context.workspace_path, &binding.pull_request);
        // Keep the existing ordering: compare with HEAD read after the provider.
        let published = git_output(&context.workspace_path, &["rev-parse", "HEAD"])?;
        let published_head = published.trim();
        let push = successful_push(host, &context.batch_id, binding, published_head)?;
        let mut differences = match &read {
            Ok(pr) => mismatches(binding, pr, published_head),
            Err(_) => vec![
                json!({"field":"observation", "expected":"readable pull_request object", "observed":{"presence":"unreadable"}}),
            ],
        };
        for (field, expected, observed) in [
            ("prepared_head_branch", binding.branch.as_str(), head),
            ("prepared_base_branch", binding.base_branch.as_str(), base),
        ] {
            if expected != observed {
                differences.push(json!({"field":field, "expected":safe_text(expected), "observed":{"presence":"present", "value":safe_text(observed)}}));
            }
        }
        // Read immutable admission again after each provider observation. The
        // explicit operator route also checks current task ownership/scope and
        // its reservation; the adopted-owner route checks those in its reader.
        let authority = (|| {
            let input = json!({"job_run_id":context.batch_id});
            if publication_admission(host, &input)?.as_ref() != Some(binding) {
                return Err(OrbitError::Execution(
                    "publication admission changed".into(),
                ));
            }
            if host.source_repair_admission(&context.batch_id)?.is_some() {
                host.validate_source_repair(&context.batch_id)?;
            }
            Ok::<_, OrbitError>(())
        })();
        if authority.is_err() {
            differences.push(json!({"field":"authority", "expected":"current owner task/scope/admission", "observed":{"presence":"unconfirmed"}}));
        }
        if budget
            .pending_push
            .as_ref()
            .is_some_and(|(head, proof)| head != published_head || proof != &push)
        {
            differences.push(json!({"field":"successful_push", "expected":"unchanged candidate and successful push proof", "observed":{"presence":"changed"}}));
        }
        let previous = push
            .as_ref()
            .and_then(|push| replacement_previous(push, published_head));
        let stale = read.as_ref().ok().is_some_and(|pr| {
            previous.is_some_and(|previous| pr.exact_head_sha.as_deref() == Some(previous))
        });
        let head_only = differences.len() == 1 && differences[0]["field"] == "headRefOid";
        let mut remote = Value::Null;
        // Once waiting started, convergence must still prove the branch ref.
        if (stale && head_only) || budget.rereads > 0 && differences.is_empty() {
            let reference = format!("refs/heads/{}", binding.branch);
            let probe = remote_ref_output(
                Path::new(&context.workspace_path),
                &["ls-remote", "origin", &reference],
                "source repair publication",
            );
            remote = match probe {
                Ok(probe) => {
                    let sha = probe.stdout.lines().find_map(|line| {
                        let (sha, name) = line.split_once('\t')?;
                        (name == reference).then_some(sha)
                    });
                    json!({"reference":safe_text(&reference), "sha":sha.map(safe_text), "attempts":probe.attempts, "waited_ms":probe.waited_ms, "elapsed_ms":probe.elapsed_ms})
                }
                Err(_) => json!({"reference":safe_text(&reference), "outcome":"unreadable"}),
            };
            if remote["sha"].as_str() != Some(published_head) {
                differences.push(json!({"field":"remote_branch", "expected":safe_text(published_head), "observed":remote}));
            }
            let branch = git_output(&context.workspace_path, &["branch", "--show-current"])?;
            let origin = git_output(
                &context.workspace_path,
                &["config", "--get", "remote.origin.url"],
            )?;
            if branch.trim() != binding.branch || origin.trim() != binding.remote_url {
                differences.push(json!({"field":"repository_branch", "expected":"admitted origin and checked-out branch", "observed":{"presence":"changed"}}));
            }
            if previous.is_none() {
                differences.push(json!({"field":"successful_push", "expected":"authenticated replacement push", "observed":{"presence":"unconfirmed"}}));
            }
        }
        let can_wait = stale
            && differences.len() == 1
            && head_only
            && !budget
                .settled_heads
                .iter()
                .any(|head| head == published_head)
            && budget.rereads < MAX_REREADS
            && budget.waited_seconds < MAX_REREADS * POLL_SECONDS;
        let delay = if can_wait { POLL_SECONDS } else { 0 };
        if can_wait {
            budget.rereads += 1;
            budget.waited_seconds += delay;
            budget.pending_push = Some((published_head.into(), push.clone()));
        }
        let settled = differences.is_empty();
        let observation = PublicationSettlingObservation {
            run_id: serde_json::from_value(safe_text(&context.batch_id))
                .map_err(|error| OrbitError::Execution(error.to_string()))?,
            published_head: serde_json::from_value(safe_text(published_head))
                .map_err(|error| OrbitError::Execution(error.to_string()))?,
            successful_push: push,
            reserved_delay_seconds: delay,
            decision: if can_wait {
                PublicationSettlingDecision::Waiting
            } else if settled {
                PublicationSettlingDecision::Settled
            } else {
                PublicationSettlingDecision::Refused
            },
        };
        let finished_at = Utc::now();
        let evidence = json!({
            "schema_version":1, "run_id":observation.run_id,
            "admission":admission, "admission_binding_sha256":binding_hash,
            "published_head":observation.published_head, "successful_push":observation.successful_push,
            "remote_branch":remote,
            "read":{"operation":operations::PR_STATUS, "selector":safe_text(&binding.pull_request), "started_at":started_at, "finished_at":finished_at,
                "outcome":if read.is_ok() { "readable" } else { "unreadable" },
                "failure_kind":read.as_ref().err().map(|error| if error.is_timeout() { "timeout" } else { "provider_read_failed" })},
            "mismatches":differences,
            "settling":{"max_rereads":MAX_REREADS, "max_wait_seconds":MAX_REREADS * POLL_SECONDS,
                "refusal_reason":if settled || can_wait { None } else if stale && head_only && differences.len() == 1 {
                    Some(if budget.settled_heads.iter().any(|head| head == published_head) { "head_regressed" } else { "budget_exhausted" })
                } else { Some("predicate_changed_or_unproven_head") },
                "rereads":budget.rereads, "waited_seconds":budget.waited_seconds, "reserved_delay_seconds":delay,
                "decision":observation.decision},
        });
        record_observation(host, binding, finished_at, &evidence, observation)?;
        if settled {
            return Ok(());
        }
        if can_wait {
            fixed_wait(Duration::from_secs(delay));
            continue;
        }
        if let Err(error) = read
            && error.is_timeout()
        {
            return Err(error);
        }
        let fields = differences
            .iter()
            .filter_map(|m| m["field"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OrbitError::Execution(format!(
            "source repair PR identity changed ({fields}); creating or replacing a pull request is not authorized; expected/observed evidence retained in source_repair_publication_refused history"
        )));
    }
}

fn replacement_previous<'a>(push: &'a PublicationPushProof, head: &str) -> Option<&'a str> {
    let previous = push.remote_sha_before.as_text()?;
    (push.force_with_lease == Some(true)
        && matches!(push.decision, PublicationPushDecision::Performed(_))
        && previous != head
        && matches!(previous.len(), 40 | 64)
        && previous.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then_some(previous)
}

fn record_observation<H: RuntimeHost + ?Sized>(
    host: &H,
    binding: &SourceRepairBinding,
    at: DateTime<Utc>,
    evidence: &Value,
    observation: PublicationSettlingObservation,
) -> Result<(), OrbitError> {
    host.apply_task_automation_update(
        &binding.task_id,
        TaskAutomationUpdate {
            append_history: vec![TaskHistoryEntry {
                at,
                by: "system".into(),
                event: if observation.decision == PublicationSettlingDecision::Refused {
                    REFUSED
                } else {
                    OBSERVED
                }
                .into(),
                note: Some(evidence.to_string()),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: Some(TaskDecision::SourceRepairPublication(observation)),
            }],
            ..Default::default()
        },
    )
}

fn admission_evidence(binding: &SourceRepairBinding) -> Result<Value, OrbitError> {
    let binding =
        serde_json::to_value(binding).map_err(|e| OrbitError::Execution(e.to_string()))?;
    let mut admission = serde_json::Map::new();
    for key in [
        "workspace_id",
        "task_id",
        "source_run_id",
        "machine_id",
        "source_digest",
        "task_digest",
        "repository",
        "branch",
        "pull_request",
        "pull_request_url",
        "head_sha",
        "base_sha",
        "base_branch",
    ] {
        admission.insert(key.into(), bounded_value(&binding[key]));
    }
    Ok(Value::Object(admission))
}

// Deserialize the authenticated checkpoint once, before projecting its fields.
// Arbitrary diagnostic fields remain raw only at this observation boundary.
#[derive(serde::Deserialize)]
struct PushCheckpoint {
    phase: PublicationPushCheckpoint,
    branch: String,
    local_sha: String,
    #[serde(default)]
    remote_sha_before: Value,
    #[serde(default)]
    force_with_lease: Value,
    #[serde(default)]
    decision: Value,
}

/// A checkpoint is evidence only when this run's successful step output agrees
/// with its named pipeline checkpoint and the exact branch/published head.
/// Missing or unreadable provenance is recorded as unconfirmed, never invented
/// from the caller's input or the PR answer. This does not authorize a push.
fn successful_push<H: RuntimeHost + ?Sized>(
    host: &H,
    run_id: &str,
    binding: &SourceRepairBinding,
    head: &str,
) -> Result<Option<PublicationPushProof>, OrbitError> {
    let Ok(Some(state)) = host.read_run_state(run_id) else {
        return Ok(None);
    };
    if state.run_id != run_id || state.job_id != "task_pr_pipeline" {
        return Ok(None);
    }
    let Some(push) = state.pipeline.get("push") else {
        return Ok(None);
    };
    let Ok(checkpoint) = serde_json::from_value::<PushCheckpoint>(push.clone()) else {
        return Ok(None);
    };
    if checkpoint.branch != binding.branch || checkpoint.local_sha != head {
        return Ok(None);
    }
    let Some((index, _)) = state.step_outputs.iter().find(|(index, output)| {
        *output == push && state.step_states.get(index) == Some(&JobRunState::Success)
    }) else {
        return Ok(None);
    };
    Ok(Some(PublicationPushProof {
        step_index: *index,
        checkpoint: checkpoint.phase,
        checkpoint_sha256: sha256_hex(push.to_string().as_bytes()),
        branch: proof_value(&json!(checkpoint.branch))?,
        local_sha: proof_value(&json!(checkpoint.local_sha))?,
        remote_sha_before: proof_value(&checkpoint.remote_sha_before)?,
        force_with_lease: checkpoint.force_with_lease.as_bool(),
        decision: serde_json::from_value(bounded_value(&checkpoint.decision))
            .map_err(|error| OrbitError::Execution(error.to_string()))?,
    }))
}

/// Convert only the existing fixed-depth, redacted diagnostic projection.
fn proof_value(value: &Value) -> Result<PublicationProofValue, OrbitError> {
    serde_json::from_value(bounded_value(value))
        .map_err(|error| OrbitError::Execution(error.to_string()))
}

fn observed(value: Option<&Value>) -> Value {
    match value {
        Some(value) => json!({"presence":"present", "value":bounded_value(value)}),
        None => json!({"presence":"missing"}),
    }
}

/// Fixed-depth projection of hostile provider values. Objects/arrays are never
/// copied wholesale; only a merge commit's oid can be useful identity evidence.
fn bounded_value(value: &Value) -> Value {
    match value {
        Value::String(text) => safe_text(text),
        Value::Object(object) => {
            json!({"type":"object", "oid":object.get("oid").map(|oid| match oid { Value::String(s) => safe_text(s), _ => json!({"type":"invalid"}) })})
        }
        Value::Array(_) => json!({"type":"array"}),
        other => other.clone(),
    }
}

fn safe_text(text: &str) -> Value {
    // Retain ordinary PR URLs, but no userinfo, query or fragment payloads in
    // arbitrary text, including fields that were supposed to be branch names.
    let safe = if text.contains("://") && text.contains(['@', '?', '#']) {
        "[REDACTED_LOCATION]".into()
    } else {
        redact_all(text)
    };
    const FIELD_BYTES: usize = 512;
    if safe.len() > FIELD_BYTES {
        json!({"prefix":&safe[..floor_char_boundary(&safe, FIELD_BYTES)], "bytes":safe.len(), "truncated":true})
    } else {
        json!(safe)
    }
}
