//! Provider identities establish grouping; first-parent commits establish ordering.

use super::Source;
use crate::{AutomationError, delivery::digest};
use orbit_types::workflow::automation::*;
use serde_json::Value;
use std::collections::BTreeMap;

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
        key: format!("pr:{repository}:{branch}:{number}"),
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
            evidence_reference: association.reference.clone(),
            evidence_digest,
            landed_at: association.landed_at,
        });
    }

    Ok(deliveries)
}
