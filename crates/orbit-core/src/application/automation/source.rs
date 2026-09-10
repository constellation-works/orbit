//! Bounded source facts from Git and provider-owned PR identities.

use orbit_automation::{AutomationError, delivery::digest};
use orbit_types::workflow::automation::*;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(test)]
thread_local! {
    static LS_TREE_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_ls_tree_invocations() {
    LS_TREE_INVOCATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn ls_tree_invocations() -> usize {
    LS_TREE_INVOCATIONS.with(std::cell::Cell::get)
}

pub(crate) struct Source<'a> {
    root: &'a Path,
    started: Instant,
}

impl<'a> Source<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            started: Instant::now(),
        }
    }

    /// Run one bounded child process, capturing stdout under a size and time budget.
    fn command(&self, program: &str, args: &[&str]) -> Result<String, AutomationError> {
        #[cfg(test)]
        if program == "git" && args.first() == Some(&"ls-tree") {
            LS_TREE_INVOCATIONS.with(|count| count.set(count.get() + 1));
        }

        if self.started.elapsed() > Duration::from_secs(30) {
            return Err(AutomationError::Deferred("source_deadline".into()));
        }

        let mut file =
            tempfile::tempfile().map_err(|e| AutomationError::Deferred(e.to_string()))?;
        let output = file
            .try_clone()
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;
        // Keep each value as a separate OS argument. This avoids treating the
        // collected values as a command-line string while retaining Git's
        // normal argument semantics.
        let mut command = Command::new(program);
        for arg in args {
            command.arg(arg);
        }

        let mut child = command
            .current_dir(self.root)
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| AutomationError::Deferred(format!("evidence_unavailable: {e}")))?;

        let start = Instant::now();

        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| AutomationError::Deferred(e.to_string()))?
            {
                if !status.success() {
                    return Err(AutomationError::Deferred("evidence_unavailable".into()));
                }
                break;
            }

            let over_budget = start.elapsed() > Duration::from_secs(2)
                || self.started.elapsed() > Duration::from_secs(30)
                || file
                    .metadata()
                    .map(|meta| meta.len() > 1_048_576)
                    .unwrap_or(true);

            if over_budget {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AutomationError::Deferred("source_budget".into()));
            }

            std::thread::sleep(Duration::from_millis(10));
        }

        file.seek(SeekFrom::Start(0))
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        let mut result = String::new();
        file.take(1_048_577)
            .read_to_string(&mut result)
            .map_err(|e| AutomationError::Deferred(e.to_string()))?;

        if result.len() > 1_048_576 {
            return Err(AutomationError::Deferred("source_budget".into()));
        }

        Ok(result.trim().into())
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, AutomationError> {
        self.command("git", args)
    }

    pub(crate) fn revision(&self, spec: &str) -> Result<SourceRevision, AutomationError> {
        let commit = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{spec}^{{commit}}"),
        ])?;

        let tree = self.git(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{commit}^{{tree}}"),
        ])?;

        Ok(SourceRevision { commit, tree })
    }

    pub(crate) fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        self.git(&["check-ref-format", "--branch", branch])?;
        let repository = self.repository()?;

        // Observe the configured integration ref, never the executor worktree HEAD.
        let head = self.revision(&format!("refs/heads/{branch}"))?;

        Ok((repository, head))
    }

    /// The stable repository identity shared by delivery observation and
    /// review certificates: a GitHub remote gives the provider-visible name;
    /// anything else is identified by a digest of its remote or git dir.
    pub(crate) fn repository(&self) -> Result<String, AutomationError> {
        let remote = self
            .git(&["config", "--get", "remote.origin.url"])
            .unwrap_or_default();

        let normalized = remote.trim_end_matches(".git");
        let repository = if let Some(github) = normalized
            .strip_prefix("https://github.com/")
            .or_else(|| normalized.strip_prefix("git@github.com:"))
        {
            github.to_string()
        } else {
            let identity = if remote.is_empty() {
                self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?
            } else {
                remote
            };
            format!("git:{}", digest(identity.as_bytes()))
        };

        Ok(repository)
    }

    pub(super) fn observe(
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

    pub(super) fn observe_with_lookup(
        &self,
        branch: &str,
        state: &AutomationState,
        lookup: &dyn Fn(&str, &str) -> Result<String, AutomationError>,
    ) -> Result<SourcePage, AutomationError> {
        let (repository, head) = self.head(branch)?;

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

        let through = if count > 200 {
            self.revision(&format!("{}~{}", head.commit, count - 200))?
        } else {
            head
        };

        let page_range = format!("{}..{}", state.observed.commit, through.commit);
        let commits = self
            .git(&[
                "rev-list",
                "--first-parent",
                "--reverse",
                "--max-count=200",
                &page_range,
            ])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        let mut unresolved = BTreeMap::new();
        let mut associations = state.associations.clone();

        // Each pass resolves the newest commits plus a rotating slice of the
        // previously unresolved ones, so no commit is starved of retries.
        let old = state.unresolved.keys().collect::<Vec<_>>();
        let offset = (state.generation as usize) % old.len().max(1);
        let candidates = commits.iter().take(10).chain(
            old.iter()
                .cycle()
                .skip(offset)
                .take(old.len().min(10))
                .copied(),
        );

        for sha in candidates {
            if self.started.elapsed() > Duration::from_secs(20) {
                break;
            }

            // Without a provider-visible repository there is no identity to ask for.
            if repository.starts_with("git:") {
                unresolved.insert(sha.clone(), "delivery_owner_evidence_pending".into());
                continue;
            }

            let raw = match lookup(&repository, sha) {
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
                [pr] => Some(super::provider::association(pr, &repository, branch)?),
                _ => {
                    unresolved.insert(sha.clone(), "provider_identity_ambiguous".into());
                    continue;
                }
            };

            associations.insert(sha.clone(), association);
            unresolved.insert(sha.clone(), "landing_span_pending".into());
        }

        let deliveries =
            super::provider::group(self, state, &repository, branch, &commits, &associations)?;

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
            unresolved,
            exclusions: Default::default(),
            complete: count <= 200,
        })
    }

    /// Pin the batch boundaries so the frozen input stays reachable during review.
    pub(super) fn retain_batch(&self, batch: &CoverageBatch) -> Result<(), AutomationError> {
        let prefix = format!(
            "refs/orbit/automation/{}/{}",
            digest(batch.consumer.as_bytes()),
            batch.id
        );

        self.git(&[
            "update-ref",
            &format!("{prefix}/from"),
            &batch.from_exclusive.commit,
        ])?;

        self.git(&[
            "update-ref",
            &format!("{prefix}/through"),
            &batch.through_inclusive.commit,
        ])?;

        Ok(())
    }

    pub(super) fn verify_batch(&self, batch: &CoverageBatch) -> Result<(), AutomationError> {
        if self.revision(&batch.from_exclusive.commit)? != batch.from_exclusive
            || self.revision(&batch.through_inclusive.commit)? != batch.through_inclusive
        {
            return Err(AutomationError::Deferred("source_revision_changed".into()));
        }

        let (_, head) = self.head(&batch.branch)?;

        self.git(&[
            "merge-base",
            "--is-ancestor",
            &batch.through_inclusive.commit,
            &head.commit,
        ])
        .map_err(|_| AutomationError::Deferred("history_diverged".into()))?;

        let range = format!(
            "{}..{}",
            batch.from_exclusive.commit, batch.through_inclusive.commit
        );

        let actual = self
            .git(&["rev-list", "--first-parent", "--reverse", &range])?
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();

        if actual != batch.commits {
            return Err(AutomationError::Deferred(
                "source_membership_changed".into(),
            ));
        }

        Ok(())
    }
}
