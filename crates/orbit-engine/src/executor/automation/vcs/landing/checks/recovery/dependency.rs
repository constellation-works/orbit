//! Dependency waiters: failed jobs whose only diagnostic is an authenticated
//! dependency record take the class of the jobs they name.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::context::{HandoffLandingContext, RuntimeHost};

use super::super::super::super::operations;

use super::{CheckFailureClass, FailedJob};

/// Dependency job ids a waiter's record names, parsed without trusting
/// them: only a gate for spending the authenticated read, never evidence.
fn dependency_ids_in_log(log: &str) -> Vec<u64> {
    let Some((_, body)) = log.split_once("orbit-dependency-evidence:") else {
        return Vec::new();
    };
    let Some(record) = body
        .lines()
        .next()
        .and_then(|line| serde_json::from_str::<Value>(line.trim()).ok())
    else {
        return Vec::new();
    };
    record["dependencies"]
        .as_array()
        .map(|deps| {
            deps.iter()
                .filter_map(|dep| dep["job_id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// What authenticating a waiter's dependency record reads through the
/// provider adapter: the landing's host, checkout, candidate, PR, head and
/// accepted base, bounded by one deadline.
pub(super) struct DependencyAuthScope<'a, H: RuntimeHost + ?Sized> {
    pub(super) host: &'a H,
    pub(super) workspace: &'a std::path::Path,
    pub(super) context: &'a HandoffLandingContext,
    pub(super) pr: u64,
    pub(super) head: &'a str,
    pub(super) base: &'a str,
    pub(super) deadline_ms: u64,
}

/// Fold authenticated dependency waiters into the jobs they name: a waiter
/// whose only diagnostic is an authenticated dependency record takes no
/// class of its own, so the aggregate class comes from its dependencies.
/// An unauthenticated or malformed record keeps its classified class.
pub(super) fn fold_dependency_waiters<H: RuntimeHost + ?Sized>(
    scope: &DependencyAuthScope<'_, H>,
    waiter_logs: &[(u64, String)],
    jobs: &mut [FailedJob],
    dependency_auth: &mut BTreeMap<(u64, u64), Option<Vec<u64>>>,
) {
    let waiter_ids: BTreeSet<u64> = waiter_logs.iter().map(|(id, _)| *id).collect();
    let mut folded: BTreeSet<u64> = BTreeSet::new();
    for _ in 0..waiter_logs.len() {
        let mut progressed = false;
        for (waiter_id, waiter_log) in waiter_logs {
            if folded.contains(waiter_id) {
                continue;
            }
            // Authentication re-reads provider logs through the adapter, so
            // only spend it when folding can change the deciding class: a
            // dependency the waiter masks is transient or candidate. A
            // waiter beside an inherited or otherwise waiting dependency
            // keeps today's route and spends no extra read.
            let named: Vec<u64> = dependency_ids_in_log(waiter_log);
            let masks_deciding = jobs.iter().any(|job| {
                named.contains(&job.job_id)
                    && matches!(
                        job.class,
                        CheckFailureClass::Transient | CheckFailureClass::Candidate
                    )
            });
            if !masks_deciding {
                continue;
            }
            let Some(run_id) = jobs
                .iter()
                .find(|job| job.job_id == *waiter_id)
                .and_then(|job| job.run_id)
            else {
                continue;
            };
            let dependency_ids = match dependency_auth.get(&(run_id, *waiter_id)) {
                Some(cached) => cached.clone(),
                None => {
                    let authenticated =
                        authenticated_dependency_ids(scope, run_id, *waiter_id, jobs);
                    dependency_auth.insert((run_id, *waiter_id), authenticated.clone());
                    authenticated
                }
            };
            let Some(dependency_ids) = dependency_ids else {
                continue;
            };
            // A chained waiter folds only after its own dependencies have.
            if dependency_ids.iter().any(|id| {
                !jobs.iter().any(|job| job.job_id == *id)
                    || (waiter_ids.contains(id) && !folded.contains(id))
            }) {
                continue;
            }
            let dependencies: Vec<&FailedJob> = jobs
                .iter()
                .filter(|job| dependency_ids.contains(&job.job_id))
                .collect();
            if dependencies.is_empty()
                || dependencies
                    .iter()
                    .any(|job| job.class == CheckFailureClass::Unknown)
            {
                continue;
            }
            let class = dependencies
                .iter()
                .map(|job| job.class)
                .max()
                .unwrap_or(CheckFailureClass::Candidate);
            let labels = dependencies
                .iter()
                .map(|job| format!("'{}'", job.label))
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(waiter) = jobs.iter_mut().find(|job| job.job_id == *waiter_id) {
                waiter.class = class;
                waiter.why = format!(
                    "folded into {labels}: its only diagnostic is an authenticated \
                     dependency record naming that job, so it takes its dependencies' class"
                );
            }
            folded.insert(*waiter_id);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

/// Authenticate one waiter's dependency record through the same adapter the
/// rerun planner uses, returning the dependency job ids it names. Any
/// failure — malformed, forged or unproven closure — returns `None`.
fn authenticated_dependency_ids<H: RuntimeHost + ?Sized>(
    scope: &DependencyAuthScope<'_, H>,
    run_id: u64,
    waiter_id: u64,
    jobs: &[FailedJob],
) -> Option<Vec<u64>> {
    let DependencyAuthScope {
        host,
        workspace,
        context,
        pr,
        head,
        base,
        deadline_ms,
    } = *scope;
    let roots: Vec<orbit_types::workflow::CiRerunRoot> = jobs
        .iter()
        .filter(|job| {
            job.run_id == Some(run_id)
                && job.job_id != waiter_id
                && job.class != CheckFailureClass::Unknown
                && !job.signature.is_empty()
        })
        .map(|job| {
            // The planner authenticates a stopped job only as a transient root
            // carrying its conclusion signature; a log-classified job stays flaky.
            let classification = if super::super::jobs::is_conclusion_signature(&job.signature) {
                CheckFailureClass::Transient.as_str()
            } else {
                "flaky"
            };
            orbit_types::workflow::CiRerunRoot {
                job_id: job.job_id,
                check: job.label.clone(),
                classification: classification.to_string(),
                signature: job.signature.clone(),
            }
        })
        .collect();
    if roots.is_empty() {
        return None;
    }
    let answer = host
        .run_private_vcs_operation(
            operations::CI_RERUN_PLAN,
            json!({
                "workspace_path": workspace,
                "pr": pr.to_string(),
                "head_sha": head,
                "head_branch": context.candidate.source_branch,
                "base_branch": context.candidate.base_branch,
                "accepted_base": base,
                "run_id": run_id,
                "roots": roots,
                "deadline_ms": deadline_ms,
            }),
        )
        .ok()?;
    let dependencies = answer["plan"]["authentication"]["dependencies"].as_array()?;
    let record = dependencies
        .iter()
        .find(|record| record["job_id"].as_u64() == Some(waiter_id))?;
    let ids: Vec<u64> = record["dependency_ids"]
        .as_array()?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

/// The unclassified reason for logs that could not be read, one job at a time.
pub(in crate::executor::automation::vcs::landing::checks) fn unreadable_job_text(
    jobs: &[FailedJob],
) -> String {
    let text = jobs
        .iter()
        .filter(|job| job.class == CheckFailureClass::Unknown)
        .map(|job| format!("the log of '{}' could not be read: {}", job.label, job.why))
        .collect::<Vec<_>>()
        .join("; ");
    if text.is_empty() {
        "no readable failed-job log decided the classification".to_string()
    } else {
        text
    }
}
