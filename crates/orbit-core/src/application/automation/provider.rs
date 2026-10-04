//! Provider identities establish grouping; first-parent commits establish ordering.

use super::source::Source;
use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;
use orbit_automation::{AutomationError, delivery::digest};
use orbit_common::OrbitError;
use orbit_types::task::ExternalRef;
use orbit_types::workflow::automation::*;
use serde_json::Value;
use std::collections::BTreeMap;

const PR_KEY_PREFIX: &str = "pr:";

/// Bounds one PR's attribution read; a bundle holds far fewer tasks.
const MAX_LANDING_TASKS: usize = 100;

pub(super) fn association(
    pr: &Value,
    repository: &str,
    branch: &str,
) -> Result<DeliveryAssociation, AutomationError> {
    let invalid = || AutomationError::Deferred("provider_identity_missing".into());

    let reference = pr["html_url"].as_str().ok_or_else(invalid)?.to_string();
    let anchor = pr["merge_commit_sha"]
        .as_str()
        .ok_or_else(invalid)?
        .to_string();
    let number = pr["number"].as_u64().ok_or_else(invalid)?;
    let landed_at = pr["merged_at"]
        .as_str()
        .and_then(|merged_at| chrono::DateTime::parse_from_rfc3339(merged_at).ok())
        .map(|merged_at| merged_at.with_timezone(&chrono::Utc))
        .ok_or_else(invalid)?;

    Ok(DeliveryAssociation {
        key: format!("{PR_KEY_PREFIX}{repository}:{branch}:{number}"),
        anchor,
        reference,
        landed_at,
    })
}

pub(super) fn group(
    source: &Source<'_>,
    state: &AutomationState,
    repository: &str,
    branch: &str,
    commits: &[String],
    associations: &BTreeMap<String, Option<DeliveryAssociation>>,
) -> Result<Vec<Delivery>, AutomationError> {
    let pending = state
        .pending_commits
        .iter()
        .chain(commits)
        .collect::<Vec<_>>();

    let mut deliveries = vec![];

    // Each association's anchor commit closes a delivery; walking back over the
    // commits sharing that association gives the group it landed as.
    for (end, sha) in pending.iter().enumerate() {
        let Some(Some(association)) = associations.get(*sha) else {
            continue;
        };

        if state
            .pending
            .iter()
            .chain(&state.waived)
            .any(|delivery| delivery.key == association.key)
        {
            continue;
        }

        if deliveries.len() == 50 {
            break;
        }

        if &association.anchor != *sha {
            continue;
        }

        // A commit with no association at all leaves the group's start unknown,
        // so the whole delivery is withheld rather than guessed at.
        let mut start = end;
        let mut unresolved = false;

        while start > 0 {
            match associations.get(pending[start - 1]) {
                Some(Some(previous)) if previous.key == association.key => start -= 1,
                Some(_) => break,
                None => {
                    unresolved = true;
                    break;
                }
            }
        }

        if unresolved {
            continue;
        }

        let before = source.revision(&format!("{}^1", pending[start]))?;
        let after = source.revision(sha)?;

        if before.tree == after.tree {
            continue;
        }

        let members = pending[start..=end]
            .iter()
            .map(|sha| (*sha).clone())
            .collect::<Vec<_>>();

        let evidence_digest = digest(
            &serde_json::to_vec(&(association, &members, &before, &after))
                .map_err(|e| AutomationError::Evidence(e.to_string()))?,
        );

        deliveries.push(Delivery {
            key: association.key.clone(),
            repository: repository.into(),
            branch: branch.into(),
            before,
            after,
            commits: members,
            task_ids: vec![],
            unattributed: None,
            evidence_reference: association.reference.clone(),
            evidence_digest,
            landed_at: association.landed_at,
        });
    }

    Ok(deliveries)
}

/// Attribute each newly observed PR delivery to the tasks whose landing record
/// names that PR. Promotion stamps `github-pr:<number>` on every member of the
/// bundle it opened, before the merge, so a bundle lists all of its members.
/// Commit text is never read. A PR no task records is marked unattributed
/// rather than left looking like a fact recorded before attribution existed.
///
/// The PR number alone identifies the PR: a workspace's tasks belong to the
/// one repository its delivery consumers observe.
pub(super) fn attribute(
    runtime: &OrbitRuntime,
    deliveries: &mut [Delivery],
) -> Result<(), AutomationError> {
    for delivery in deliveries {
        if !delivery.task_ids.is_empty() || delivery.unattributed.is_some() {
            continue;
        }
        let Some(number) = pull_request_number(&delivery.key) else {
            continue;
        };
        if !runtime.coordination_task_reads_visible() {
            delivery.unattributed = Some(UNATTRIBUTED_TASKS_UNREADABLE.into());
            continue;
        }

        // An unreadable task store defers the pass: freezing "no task" for a
        // landing that has one would be a wrong attribution, not a gap.
        let filter = TaskListFilter {
            external_ref: Some(ExternalRef::github_pr(number).map_err(OrbitError::from)?),
            ..TaskListFilter::default()
        };
        let mut task_ids = runtime
            .task_candidates(&filter, MAX_LANDING_TASKS)
            .map_err(|error| {
                AutomationError::Deferred(format!("task_records_unavailable: {error}"))
            })?
            .items
            .into_iter()
            .map(|envelope| envelope.id.to_string())
            .collect::<Vec<_>>();
        task_ids.sort();
        task_ids.dedup();

        delivery.unattributed = task_ids
            .is_empty()
            .then(|| UNATTRIBUTED_NO_LANDING_TASK.into());
        delivery.task_ids = task_ids;
    }

    Ok(())
}

/// The PR number of a key [`association`] built; branch and repository names
/// cannot contain `:`.
fn pull_request_number(key: &str) -> Option<&str> {
    key.strip_prefix(PR_KEY_PREFIX)?
        .rsplit_once(':')
        .map(|(_, number)| number)
}
