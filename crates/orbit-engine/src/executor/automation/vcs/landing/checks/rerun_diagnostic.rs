//! Bounded observations, never authorization for a CI mutation.
use super::super::super::pr::PullRequestStatus;
use crate::context::RuntimeHost;
use orbit_common::{OrbitError, security::redaction::redact_all_and_home};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_OBSERVATIONS: usize = 16;
const MAX_JOBS: usize = 32;
const MAX_REASON_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::executor::automation::vcs) enum CiRerunStage {
    #[default]
    Identity,
    Jobs,
    Log,
    Classification,
    Authentication,
    PlanIdentity,
    Admission,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CiRerunDiagnosticKind {
    Wait,
    EvidenceUnavailable,
    PolicyDenied,
    DeliveryRefused,
    TransientObservation,
    Transient,
    CompletionObservationUnavailable,
    ProviderRefusal,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct CiRerunJobIdentity {
    run_id: Option<u64>,
    job_id: Option<u64>,
    run_attempt: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(in crate::executor::automation::vcs) struct CiRerunDiagnostic {
    pub(super) stage: CiRerunStage,
    pub(super) pr_number: Option<u64>,
    pub(in crate::executor::automation::vcs) head_sha: Option<String>,
    pub(super) run_id: Option<u64>,
    pub(super) job_id: Option<u64>,
    jobs: Vec<CiRerunJobIdentity>,
    jobs_omitted: usize,
    kind: CiRerunDiagnosticKind,
    reason: String,
}

impl CiRerunDiagnostic {
    pub(super) fn for_status(status: &PullRequestStatus) -> Self {
        Self {
            stage: CiRerunStage::Identity,
            pr_number: status.number,
            head_sha: status
                .head_sha
                .as_ref()
                .filter(|head| head.len() <= 128)
                .cloned(),
            run_id: None,
            job_id: None,
            jobs: Vec::new(),
            jobs_omitted: 0,
            kind: CiRerunDiagnosticKind::Wait,
            reason: String::new(),
        }
    }

    pub(super) fn bind_jobs(&mut self, jobs: &[Value]) {
        let mut identities = jobs
            .iter()
            .filter(|job| super::jobs::unsuccessful(job))
            .map(|job| CiRerunJobIdentity {
                run_id: job["run_id"].as_u64(),
                job_id: job["job_id"].as_u64(),
                run_attempt: job["run_attempt"].as_u64(),
            })
            .collect::<Vec<_>>();
        identities.sort();
        identities.dedup();
        self.jobs_omitted = identities.len().saturating_sub(MAX_JOBS);
        identities.truncate(MAX_JOBS);
        self.jobs = identities;
    }

    pub(super) fn with_reason(mut self, reason: &str) -> Self {
        self.reason = bounded_ci_rerun_reason(reason);
        self
    }

    pub(super) fn with_error(self, error: &OrbitError) -> Self {
        let mut diagnostic = self;
        diagnostic.kind = match error {
            OrbitError::PrivateVcs(failure) => private_vcs_diagnostic_kind(failure.class),
            OrbitError::DeliveryRefused { .. } => CiRerunDiagnosticKind::DeliveryRefused,
            OrbitError::PolicyDenied(_) => CiRerunDiagnosticKind::PolicyDenied,
            _ => CiRerunDiagnosticKind::EvidenceUnavailable,
        };
        diagnostic.with_reason(&error.to_string())
    }
}

/// Keep the diagnostic header, never raw multiline provider output. Redact
/// before UTF-8 truncation so a token crossing the bound cannot leak a prefix.
fn bounded_ci_rerun_reason(reason: &str) -> String {
    let mut reason = redact_all_and_home(reason.lines().next().unwrap_or_default());
    if reason.len() > MAX_REASON_BYTES {
        let mut end = MAX_REASON_BYTES - 3;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
        reason.push_str("...");
    }
    reason
}

#[derive(Default, Deserialize, PartialEq, Serialize)]
struct CiRerunJournal {
    schema_version: u8,
    execution: String,
    observed_head: Option<String>,
    current: Option<CiRerunDiagnostic>,
    observations: Vec<Option<CiRerunDiagnostic>>,
    omitted_observations: u64,
    outcome: Option<Value>,
}

/// One replaceable artifact per execution, no append-only history. Read it
/// back on resume to deduplicate unchanged refusals across invocations too.
/// The ring preserves cause/head transitions; current is cleared by a later
/// successful planner read, and always scoped to observed_head.
fn update_ci_rerun_journal<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    execution: &str,
    update: impl Fn(&mut CiRerunJournal),
) -> Result<(), OrbitError> {
    let path = format!("ci-rerun/{execution}/diagnostics.json");
    for task in tasks {
        let prior = host.get_task_artifact(task, &path)?;
        let mut journal = match prior.as_ref() {
            Some(artifact) => {
                serde_json::from_slice::<CiRerunJournal>(&artifact.content).map_err(|_| {
                    OrbitError::Execution("unreadable CI retry diagnostic journal".into())
                })?
            }
            None => CiRerunJournal {
                schema_version: 1,
                execution: execution.into(),
                ..Default::default()
            },
        };
        if journal.schema_version != 1 || journal.execution != execution {
            return Err(OrbitError::Execution(
                "CI retry diagnostic execution differs".into(),
            ));
        }
        let before = serde_json::to_vec(&journal)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        update(&mut journal);
        // No artifact is needed for an execution that saw no refusal.
        if journal.observations.is_empty() {
            continue;
        }
        let content = serde_json::to_vec(&journal)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if content == before {
            continue;
        }
        let owner = host
            .get_task(task)?
            .job_run_id
            .ok_or_else(|| OrbitError::Execution("CI retry diagnostic has no owning run".into()))?;
        host.attach_task_validation_log(task, &owner, &path, content)?;
    }
    Ok(())
}

pub(in crate::executor::automation::vcs) fn record_ci_rerun_observation<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    execution: &str,
    head: Option<&str>,
    diagnostic: Option<&CiRerunDiagnostic>,
) {
    let recorded = update_ci_rerun_journal(host, tasks, execution, |journal| {
        if journal.current.as_ref() != diagnostic {
            journal.observations.push(diagnostic.cloned());
            if journal.observations.len() > MAX_OBSERVATIONS {
                journal.observations.remove(1);
                journal.omitted_observations = journal.omitted_observations.saturating_add(1);
            }
        }
        journal.observed_head = head.filter(|head| head.len() <= 128).map(ToOwned::to_owned);
        journal.current = diagnostic.cloned();
        journal.outcome = None;
    });
    if let Err(error) = recorded {
        // Persistence failure grants no mutation permission and must not
        // replace the planner's original refusal or completion's typed hold.
        tracing::warn!(error = %redact_all_and_home(&error.to_string()), "CI retry diagnostic could not be retained");
    }
}

/// Invalidate a persisted prior-head refusal even on a resumed invocation
/// that stops at the pin check before it can call the planner again.
pub(in crate::executor::automation::vcs) fn observe_ci_rerun_head<H: RuntimeHost + ?Sized>(
    host: &H,
    tasks: &[String],
    execution: &str,
    head: Option<&str>,
    has_failures: bool,
) {
    if let Err(error) = update_ci_rerun_journal(host, tasks, execution, |journal| {
        if journal
            .current
            .as_ref()
            .is_some_and(|current| current.head_sha.as_deref() != head || !has_failures)
        {
            journal.current = None;
            journal.observations.push(None);
            if journal.observations.len() > MAX_OBSERVATIONS {
                journal.observations.remove(1);
                journal.omitted_observations = journal.omitted_observations.saturating_add(1);
            }
        }
        journal.observed_head = head.filter(|head| head.len() <= 128).map(ToOwned::to_owned);
    }) {
        tracing::warn!(error = %redact_all_and_home(&error.to_string()), "CI retry head observation could not be retained");
    }
}

pub(in crate::executor::automation::vcs) fn record_ci_rerun_outcome<H: RuntimeHost + ?Sized, T>(
    host: &H,
    tasks: &[String],
    execution: &str,
    result: &Result<T, OrbitError>,
) {
    let outcome = match result {
        Ok(_) => json!({"kind":"completed"}),
        Err(OrbitError::DeliveryRefused { refusal, .. }) => json!({"kind":refusal.error_code()}),
        Err(OrbitError::PrivateVcs(failure)) => {
            json!({"kind":private_vcs_diagnostic_kind(failure.class)})
        }
        Err(_) => json!({"kind":"stopped"}),
    };
    if let Err(error) = update_ci_rerun_journal(host, tasks, execution, |journal| {
        journal.outcome = Some(outcome.clone());
    }) {
        tracing::warn!(error = %redact_all_and_home(&error.to_string()), "CI retry outcome could not be retained");
    }
}

/// Preserve the existing error variant/class and its completion deadline.
pub(in crate::executor::automation::vcs) fn append_ci_rerun_diagnostic(
    error: OrbitError,
    diagnostic: Option<&CiRerunDiagnostic>,
) -> OrbitError {
    let Some(diagnostic) = diagnostic else {
        return error;
    };
    let suffix = format!("; CI retry diagnostic: {}", json!(diagnostic));
    match error {
        OrbitError::DeliveryRefused { refusal, message } => OrbitError::DeliveryRefused {
            refusal,
            message: format!("{message}{suffix}"),
        },
        OrbitError::Execution(message) => OrbitError::Execution(format!("{message}{suffix}")),
        OrbitError::PrivateVcs(mut failure) => {
            failure.message.push_str(&suffix);
            OrbitError::PrivateVcs(failure)
        }
        other => other,
    }
}

fn private_vcs_diagnostic_kind(
    class: orbit_common::PrivateVcsFailureClass,
) -> CiRerunDiagnosticKind {
    use orbit_common::PrivateVcsFailureClass;
    match class {
        PrivateVcsFailureClass::TransientObservation => CiRerunDiagnosticKind::TransientObservation,
        PrivateVcsFailureClass::Transient => CiRerunDiagnosticKind::Transient,
        PrivateVcsFailureClass::CompletionObservationUnavailable => {
            CiRerunDiagnosticKind::CompletionObservationUnavailable
        }
        PrivateVcsFailureClass::Other => CiRerunDiagnosticKind::ProviderRefusal,
    }
}
