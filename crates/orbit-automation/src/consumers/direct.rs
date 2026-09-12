//! Authorized direct delivery intents are retained before Git mutates the branch.

use crate::host::AutomationHost;
use crate::source::Source;
use crate::{AutomationError, automation_error_to_orbit, delivery::digest};
use orbit_common::OrbitError;
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

/// Retain one authorized direct landing as a delivery intent, before Git
/// rewrites the branch it was observed on. A landing that changed no tree is
/// not a delivery and is recorded as nothing.
pub fn record_direct_landing_intent<H: AutomationHost>(
    host: &H,
    request: &DirectLandingRequest,
) -> Result<(), OrbitError> {
    let source = Source::new(host.repo_root());
    let (repository, _) = source
        .head(&request.branch)
        .map_err(automation_error_to_orbit)?;
    let before = source
        .revision(&request.before_commit)
        .map_err(automation_error_to_orbit)?;
    let after = source
        .revision(&request.after_commit)
        .map_err(automation_error_to_orbit)?;

    // A landing that changed no tree is not a delivery.
    if before.tree == after.tree {
        return Ok(());
    }

    source
        .git(&["merge-base", "--is-ancestor", &before.commit, &after.commit])
        .map_err(automation_error_to_orbit)?;

    let range = format!("{}..{}", before.commit, after.commit);
    let commits = source
        .git(&["rev-list", "--first-parent", "--reverse", &range])
        .map_err(automation_error_to_orbit)?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();

    let run = host.show_job_run(&request.run_id)?;
    let task_ids = run
        .input
        .as_ref()
        .and_then(|input| input.get("task_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let evidence_digest =
        digest(&serde_json::to_vec(request).map_err(|e| OrbitError::InvalidInput(e.to_string()))?);

    host.automation_store()?
        .automation_record_delivery_intent(&Delivery {
            key: format!("direct:{repository}:{}:{}", request.branch, request.run_id),
            repository,
            branch: request.branch.clone(),
            before,
            after,
            commits,
            task_ids,
            evidence_reference: format!("run:{}:direct-landing", request.run_id),
            evidence_digest,
            landed_at: chrono::Utc::now(),
        })
}

pub(super) fn observe(
    source: &Source<'_>,
    store: &dyn AutomationStoreBackend,
    state: &AutomationState,
    page: &mut SourcePage,
) -> Result<(), AutomationError> {
    // This page's commits plus a rotating slice of the still-unresolved ones.
    let mut candidates = page.commits.iter().take(100).cloned().collect::<Vec<_>>();
    let unresolved = state.unresolved.keys().collect::<Vec<_>>();

    candidates.extend(
        unresolved
            .iter()
            .cycle()
            .skip((state.generation as usize) % unresolved.len().max(1))
            .take(unresolved.len().min(100))
            .map(|sha| (*sha).clone()),
    );

    let available = state
        .pending_commits
        .iter()
        .chain(page.commits.iter())
        .collect::<Vec<_>>();

    for delivery in
        store.automation_delivery_intents(&state.repository, &state.branch, &candidates)?
    {
        if !delivery.commits.iter().all(|sha| available.contains(&sha)) {
            continue;
        }

        // An intent is not a landing: only exact, reachable source objects qualify.
        if source
            .git(&[
                "merge-base",
                "--is-ancestor",
                &delivery.after.commit,
                &page.through.commit,
            ])
            .is_err()
        {
            continue;
        }

        if source.revision(&delivery.before.commit)? != delivery.before
            || source.revision(&delivery.after.commit)? != delivery.after
        {
            continue;
        }

        let range = format!("{}..{}", delivery.before.commit, delivery.after.commit);
        let actual = source
            .git(&["rev-list", "--first-parent", "--reverse", &range])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if actual != delivery.commits {
            continue;
        }

        for sha in &delivery.commits {
            page.unresolved.remove(sha);
        }

        // Two authorities claiming overlapping units are unresolved, never counted twice.
        if page.deliveries.iter().any(|other| {
            other
                .commits
                .iter()
                .any(|sha| delivery.commits.contains(sha))
        }) {
            return Err(AutomationError::Deferred(
                "delivery_grouping_ambiguous".into(),
            ));
        }

        page.deliveries.push(delivery);
    }

    Ok(())
}
