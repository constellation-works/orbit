//! Paged observation of a delivery branch and its provider identities.

use chrono::{DateTime, Utc};
use orbit_automation::AutomationError;
use orbit_types::workflow::automation::*;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};

use super::{ObservationLimits, Source};

impl<'a> Source<'a> {
    pub(in crate::application::automation) fn observe(
        &self,
        branch: &str,
        state: &AutomationState,
    ) -> Result<SourcePage, AutomationError> {
        self.observe_with_lookup(branch, state, &|repository, sha| {
            let request = orbit_tools::github_cli::commit_pull_requests_request(repository, sha)?;
            self.command(
                "gh",
                &request.args.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
    }

    pub(in crate::application::automation) fn observe_with_lookup(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
    ) -> Result<SourcePage, AutomationError> {
        self.observe_with_limits(
            branch,
            state,
            lookup,
            ObservationLimits {
                commits: 200,
                lookups: 10,
            },
            None,
            true,
        )
    }

    pub(super) fn observe_with_limits(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
        limits: ObservationLimits,
        head_override: Option<SourceRevision>,
        respect_backoff: bool,
    ) -> Result<SourcePage, AutomationError> {
        let (repository, configured_head) = self.head(branch)?;
        let head = head_override.unwrap_or(configured_head);

        if repository != state.repository {
            return Err(AutomationError::Deferred("repository_changed".into()));
        }

        self.git(&[
            "merge-base",
            "--is-ancestor",
            &state.observed.commit,
            &head.commit,
        ])
        .map_err(|_| AutomationError::Deferred("history_diverged".into()))?;

        let range = format!("{}..{}", state.observed.commit, head.commit);

        // Count uses bounded output and the same command deadline. Select the oldest
        // page by first-parent distance; reverse+max-count alone selects the newest.
        let count = self
            .git(&["rev-list", "--first-parent", "--count", &range])?
            .parse::<usize>()
            .map_err(|_| AutomationError::Deferred("source_count_invalid".into()))?;

        let through = if count > limits.commits {
            self.revision(&format!("{}~{}", head.commit, count - limits.commits))?
        } else {
            head
        };

        let page_range = format!("{}..{}", state.observed.commit, through.commit);
        let max_count = format!("--max-count={}", limits.commits);
        let commits = self
            .git(&[
                "rev-list",
                "--first-parent",
                "--reverse",
                &max_count,
                &page_range,
            ])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut unresolved = BTreeMap::new();
        let mut associations = state.associations.clone();
        let mut lookup_retries = state.lookup_retries.clone();

        // Filter before budgeting so recorded identities and commits in
        // backoff cannot starve eligible unresolved commits of retries.
        let eligible = |sha: &&String| {
            !associations.contains_key(*sha)
                && (!respect_backoff || lookup_due(state.lookup_retries.get(*sha), self.now))
        };
        let old = state.unresolved.keys().filter(eligible).collect::<Vec<_>>();
        let offset = (state.generation as usize) % old.len().max(1);
        let candidates = commits
            .iter()
            .filter(eligible)
            .take(limits.lookups)
            .chain(
                old.iter()
                    .cycle()
                    .skip(offset)
                    .take(old.len().min(limits.lookups))
                    .copied(),
            )
            .cloned()
            .collect::<Vec<_>>();

        for sha in &candidates {
            if self.started.elapsed() > Duration::from_secs(20) {
                break;
            }

            // Without a provider-visible repository there is no identity to ask for.
            if repository.starts_with("git:") {
                unresolved.insert(sha.clone(), "delivery_owner_evidence_pending".into());
                continue;
            }

            let attempts = lookup_retries
                .get(sha)
                .map_or(1, |retry| retry.attempts.saturating_add(1));
            lookup_retries.insert(
                sha.clone(),
                AssociationLookupRetry {
                    last_checked_at: self.now,
                    attempts,
                },
            );
            let response = if let Some(cache) = self.cache {
                cache.lookup(&repository, branch, sha, || lookup(&repository, sha))
            } else {
                lookup(&repository, sha)
            };
            let raw = match response {
                Ok(raw) => raw,
                Err(_) => {
                    unresolved.insert(sha.clone(), "evidence_unavailable".into());
                    continue;
                }
            };

            let prs: Value =
                serde_json::from_str(&raw).map_err(|e| AutomationError::Deferred(e.to_string()))?;

            // Exactly one merged pull request into this branch is an identity;
            // several are ambiguous and none simply leaves the commit unresolved.
            let matching = prs
                .as_array()
                .ok_or_else(|| AutomationError::Deferred("provider_response_invalid".into()))?
                .iter()
                .filter(|pr| {
                    pr["merged_at"].is_string()
                        && pr["base"]["ref"].as_str() == Some(branch)
                        && pr["base"]["repo"]["full_name"].as_str() == Some(&repository)
                })
                .collect::<Vec<_>>();

            let association = match matching.as_slice() {
                [] => None,
                [pr] => Some(super::super::provider::association(
                    pr,
                    &repository,
                    branch,
                )?),
                _ => {
                    unresolved.insert(sha.clone(), "provider_identity_ambiguous".into());
                    continue;
                }
            };

            associations.insert(sha.clone(), association);
            lookup_retries.remove(sha);
            unresolved.insert(sha.clone(), "landing_span_pending".into());
        }

        let deliveries = super::super::provider::group(
            self,
            state,
            &repository,
            branch,
            &commits,
            &associations,
        )?;

        for sha in &commits {
            if !deliveries
                .iter()
                .any(|delivery| delivery.commits.contains(sha))
            {
                unresolved
                    .entry(sha.clone())
                    .or_insert_with(|| "evidence_pending".into());
            }
        }

        Ok(SourcePage {
            from: state.observed.clone(),
            through,
            commits,
            deliveries,
            associations,
            lookup_retries,
            unresolved,
            exclusions: Default::default(),
            complete: count <= limits.commits,
        })
    }
}

/// Missing identities retry after one minute, then five, then thirty.
fn lookup_due(retry: Option<&AssociationLookupRetry>, now: DateTime<Utc>) -> bool {
    let Some(retry) = retry else {
        return true;
    };
    let minutes = match retry.attempts {
        0 | 1 => 1,
        2 => 5,
        _ => 30,
    };
    now.signed_duration_since(retry.last_checked_at) >= chrono::Duration::minutes(minutes)
}
