//! Revisions, the fetched branch head and remote observation.

use orbit_automation::{AutomationError, delivery::digest};
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, git_fetch_cas_retry_delay, git_fetch_lock_target,
    should_retry_git_ref_cas,
};
use orbit_common::fs::{file_lock::FileLockOptions, io::with_exclusive_file_lock_options};
use orbit_types::workflow::automation::*;
use std::{path::Path, time::Duration};

use super::{SOURCE_DEADLINE, SOURCE_FETCH_FAILED, Source};

impl<'a> Source<'a> {
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

    /// Resolve the configured integration branch to a commit, never the
    /// executor worktree HEAD. The failure names the ref and git's own text:
    /// a branch that does not exist is a definition error an operator has to
    /// fix, and the reason has to say so wherever it is surfaced.
    ///
    /// This reads `refs/heads` only. Delivery observation uses [`Self::head`],
    /// which follows `origin` when that remote exists.
    pub(crate) fn verify_branch(&self, branch: &str) -> Result<SourceRevision, AutomationError> {
        self.git(&["check-ref-format", "--branch", branch])?;
        self.revision(&format!("refs/heads/{branch}"))
    }

    /// The local branch head, without fetching. Direct-landing intents and
    /// read-only inspection must not depend on the network.
    pub(crate) fn local_head(
        &self,
        branch: &str,
    ) -> Result<(String, SourceRevision), AutomationError> {
        let head = self.verify_branch(branch)?;
        let repository = self.repository()?;
        Ok((repository, head))
    }

    /// Select a state pilot's source without moving the primary checkout.
    /// Fetch uses the delivery observer's shared lock, but unlike delivery
    /// observation a failed fetch falls back to the local head captured first,
    /// even if the fetch consumed the source deadline. A local branch already
    /// ahead of origin stays current rather than preparing older material.
    pub(crate) fn preparation_head(
        &self,
        branch: &str,
    ) -> Result<(String, SourceRevision), AutomationError> {
        let (repository, local) = self.local_head(branch)?;
        let head = match self.fetched_head(branch) {
            Ok(origin) => {
                if origin == local
                    || self
                        .git(&["merge-base", "--is-ancestor", &origin.commit, &local.commit])
                        .is_ok()
                {
                    local
                } else {
                    origin
                }
            }
            Err(error) => {
                tracing::warn!(%branch, %error, "pilot source fetch unavailable; using local head");
                local
            }
        };
        Ok((repository, head))
    }

    /// The head a delivery consumer observes.
    ///
    /// When `origin` is configured, the pass fetches that one branch into
    /// `refs/remotes/origin/<branch>` and resolves the fetched object. The
    /// worktree, index and local branch stay untouched. A failed fetch defers
    /// as [`SOURCE_FETCH_FAILED`] and does not fall back to the local ref.
    /// With no remote, the head is `refs/heads/<branch>`. A read-only source
    /// resolves the existing remote-tracking ref without fetching.
    pub(crate) fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError> {
        self.git(&["check-ref-format", "--branch", branch])?;
        let repository = self.repository()?;
        let resolve = || self.fetched_head(branch);
        let head = if let Some(cache) = self.cache {
            let git_dir = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
            cache.head(git_dir.into(), &repository, branch, resolve)?
        } else {
            resolve()?
        };
        #[cfg(test)]
        expire_source_after_head(self);
        Ok((repository, head))
    }

    fn fetched_head(&self, branch: &str) -> Result<SourceRevision, AutomationError> {
        if self.origin_url()?.is_some() {
            if self.fetch_origin {
                self.fetch_origin_branch(branch)?;
            }
            self.revision(&format!("refs/remotes/origin/{branch}"))
                .map_err(fetch_failure_from)
        } else {
            self.revision(&format!("refs/heads/{branch}"))
        }
    }

    /// `remote.origin.url` when the repository has one.
    ///
    /// A missing key is "no remote". A deadline or any other source failure
    /// propagates, so a timed-out config read is not treated as a `git:`
    /// identity.
    fn origin_url(&self) -> Result<Option<String>, AutomationError> {
        match self.git(&["config", "--get", "remote.origin.url"]) {
            Ok(url) if !url.is_empty() => Ok(Some(url)),
            Ok(_) => Ok(None),
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Fetch `origin/<branch>` into the remote-tracking ref and nothing else.
    ///
    /// The refspec is forced (`+`) so a non-fast-forward still lands and the
    /// ancestry check can report `history_diverged`. The fetch uses whatever
    /// remains of the source deadline, not the two-second local-command budget.
    fn fetch_origin_branch(&self, branch: &str) -> Result<(), AutomationError> {
        let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        // Resolve through the bounded source runner too: the shared helper's
        // unbounded rev-parse would otherwise sit outside this pass's budget.
        let common = self.git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let options = FileLockOptions {
            timeout: self.remaining_budget()?,
            ..FileLockOptions::default()
        };
        match with_exclusive_file_lock_options(
            &git_fetch_lock_target(Path::new(&common)),
            "git fetch",
            options,
            || self.fetch_origin_branch_locked(&spec),
        ) {
            Ok(()) => Ok(()),
            Err(FetchLockError::Io(error)) => Err(fetch_failure(&error.to_string())),
            Err(FetchLockError::Deferred(error)) => Err(error),
        }
    }

    fn remaining_budget(&self) -> Result<Duration, AutomationError> {
        SOURCE_DEADLINE
            .checked_sub(self.started.elapsed())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| AutomationError::Deferred("source_deadline".into()))
    }

    fn fetch_origin_branch_locked(&self, spec: &str) -> Result<(), FetchLockError> {
        let args = [
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "origin",
            spec,
        ];
        let mut last_error = AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
        for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
            let budget = self.remaining_budget().map_err(FetchLockError::Deferred)?;
            match self.command_with("git", &args, &[("GIT_TERMINAL_PROMPT", "0")], None, budget) {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let text = match &error {
                        AutomationError::Deferred(reason) => reason.clone(),
                        other => other.to_string(),
                    };
                    let retry = should_retry_git_ref_cas(attempt, &text);
                    last_error = error;
                    if retry {
                        tracing::warn!(
                            attempt,
                            spec,
                            "retrying delivery source fetch after git ref update contention"
                        );
                        std::thread::sleep(git_fetch_cas_retry_delay());
                        continue;
                    }
                    break;
                }
            }
        }
        Err(FetchLockError::Deferred(fetch_failure_from(last_error)))
    }

    /// How far `observed` trails `refs/remotes/origin/<branch>`, without fetching.
    ///
    /// `None` when the repository has no origin. A missing remote-tracking ref
    /// is [`RemoteObservation::unfetched`], not an error, so doctor can name it.
    pub(crate) fn remote_observation(
        &self,
        branch: &str,
        observed: Option<&str>,
    ) -> Result<Option<RemoteObservation>, AutomationError> {
        if self.origin_url()?.is_none() {
            return Ok(None);
        }
        let remote = match self.revision(&format!("refs/remotes/origin/{branch}")) {
            Ok(revision) => revision.commit,
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => {
                return Ok(Some(RemoteObservation {
                    observed: observed.map(str::to_owned),
                    remote_head: None,
                    behind: None,
                    diverged: false,
                    unfetched: true,
                    oldest_unobserved_epoch: None,
                }));
            }
            Err(error) => return Err(error),
        };
        let Some(observed) = observed else {
            return Ok(Some(RemoteObservation {
                observed: None,
                remote_head: Some(remote),
                behind: None,
                diverged: false,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        };
        if observed == remote {
            return Ok(Some(RemoteObservation {
                observed: Some(observed.to_owned()),
                remote_head: Some(remote),
                behind: Some(0),
                diverged: false,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        }
        if !self.is_ancestor(observed, &remote)? {
            return Ok(Some(RemoteObservation {
                observed: Some(observed.to_owned()),
                remote_head: Some(remote),
                behind: None,
                diverged: true,
                unfetched: false,
                oldest_unobserved_epoch: None,
            }));
        }

        let range = format!("{observed}..{remote}");
        let behind = self
            .git(&["rev-list", "--first-parent", "--count", &range])?
            .parse::<u64>()
            .map_err(|_| AutomationError::Deferred("source_count_invalid".into()))?;
        let oldest_unobserved_epoch = if behind == 0 {
            None
        } else {
            // Git applies max-count before reverse, which would select the tip.
            // First-parent distance selects the oldest pending commit while
            // keeping command output constant-sized, even for a long trail.
            let oldest = format!("{remote}~{}", behind - 1);
            self.git(&["show", "-s", "--format=%ct", &oldest])?
                .parse::<i64>()
                .ok()
        };
        Ok(Some(RemoteObservation {
            observed: Some(observed.to_owned()),
            remote_head: Some(remote),
            behind: Some(behind),
            diverged: false,
            unfetched: false,
            oldest_unobserved_epoch,
        }))
    }

    /// `true` when `older` is an ancestor of `newer`. A missing object is
    /// divergence, not success. A source deadline propagates.
    pub(crate) fn is_ancestor(&self, older: &str, newer: &str) -> Result<bool, AutomationError> {
        match self.git(&["merge-base", "--is-ancestor", older, newer]) {
            Ok(_) => Ok(true),
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The tree of a conflict-free merge of `theirs` onto `ours` with `base`
    /// as the merge base, written to the object store only. `None` when the
    /// merge conflicts or an input does not resolve. A source deadline
    /// propagates.
    pub(crate) fn clean_merge_tree(
        &self,
        base: &str,
        ours: &str,
        theirs: &str,
    ) -> Result<Option<String>, AutomationError> {
        let base = format!("--merge-base={base}");
        match self.git(&[
            "merge-tree",
            "--write-tree",
            "--no-messages",
            &base,
            "--end-of-options",
            ours,
            theirs,
        ]) {
            Ok(tree) => Ok(Some(tree)),
            Err(AutomationError::Deferred(reason)) if is_evidence_unavailable(&reason) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The stable repository identity shared by delivery observation and
    /// review certificates: a GitHub remote gives the provider-visible name;
    /// anything else is identified by a digest of its remote or git dir.
    pub(crate) fn repository(&self) -> Result<String, AutomationError> {
        let remote = self.origin_url()?.unwrap_or_default();

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
}

/// Read-only comparison of a consumer's observed commit with the
/// remote-tracking head. Doctor uses this and does not fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteObservation {
    pub observed: Option<String>,
    pub remote_head: Option<String>,
    pub behind: Option<u64>,
    pub diverged: bool,
    pub unfetched: bool,
    /// Committer time, unix seconds, of the oldest first-parent commit the
    /// observed cursor has not reached. Present only when `behind` is positive.
    pub oldest_unobserved_epoch: Option<i64>,
}

/// [`with_git_fetch_lock`] requires `From<io::Error>`. Automation errors have
/// no such conversion, so the locked fetch carries both.
enum FetchLockError {
    Io(std::io::Error),
    Deferred(AutomationError),
}

impl From<std::io::Error> for FetchLockError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn is_evidence_unavailable(reason: &str) -> bool {
    reason.starts_with("evidence_unavailable:")
}

#[cfg(test)]
thread_local! {
    static EXPIRE_AFTER_NEXT_HEAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static EXPIRED_SOURCE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(in crate::application::automation) fn arm_expire_source_after_next_head() {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| armed.set(true));
    EXPIRED_SOURCE.with(|source| source.set(0));
}

#[cfg(test)]
pub(in crate::application::automation) fn clear_expired_source() {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| armed.set(false));
    EXPIRED_SOURCE.with(|source| source.set(0));
}

#[cfg(test)]
fn expire_source_after_head(source: &Source<'_>) {
    EXPIRE_AFTER_NEXT_HEAD.with(|armed| {
        if armed.replace(false) {
            EXPIRED_SOURCE.with(|expired| expired.set(source as *const Source<'_> as usize));
        }
    });
}

#[cfg(test)]
pub(super) fn source_deadline_forced(source: &Source<'_>) -> bool {
    EXPIRED_SOURCE.with(|expired| expired.get() == source as *const Source<'_> as usize)
}

fn fetch_failure(detail: &str) -> AutomationError {
    let detail = detail.trim();
    if detail.is_empty() || detail == SOURCE_FETCH_FAILED {
        return AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
    }
    if let Some(rest) = detail.strip_prefix("source_fetch_failed:") {
        let rest = rest.trim();
        if rest.is_empty() {
            return AutomationError::Deferred(SOURCE_FETCH_FAILED.into());
        }
        return AutomationError::Deferred(format!("{SOURCE_FETCH_FAILED}: {rest}"));
    }
    AutomationError::Deferred(format!("{SOURCE_FETCH_FAILED}: {detail}"))
}

fn fetch_failure_from(error: AutomationError) -> AutomationError {
    match error {
        AutomationError::Deferred(reason) => fetch_failure(&reason),
        other => fetch_failure(&other.to_string()),
    }
}
