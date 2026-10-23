//! Local status reads need neither child processes nor a fetch. libgit2
//! understands packed refs, linked worktrees, objects and layered Git config.
//! A repository libgit2 refuses to open, such as a partial clone or one with
//! reftable refs (extensions it does not implement), keeps the bounded Git
//! commands, so its status is unchanged.

use orbit_automation::{AutomationError, DeferredReason, delivery::digest};
use orbit_types::workflow::automation::SourceRevision;

use super::Source;

impl Source<'_> {
    /// The branch head a cached read-only inspection source resolves in
    /// process: the local branch, or for `preparation` the existing
    /// remote-tracking ref when the local branch does not contain it. `None`
    /// when the caller must run its Git commands instead: a writable or
    /// uncached source, or a repository libgit2 cannot open.
    pub(super) fn inspection_head(
        &self,
        branch: &str,
        preparation: bool,
    ) -> Option<Result<(String, SourceRevision), AutomationError>> {
        if self.fetch_origin {
            return None;
        }
        let cache = self.cache?;
        cache
            .inspection(self.root.into(), branch, preparation, || {
                self.read_inspection_head(branch, preparation)
            })
            .transpose()
    }

    fn read_inspection_head(
        &self,
        branch: &str,
        preparation: bool,
    ) -> Result<Option<(String, SourceRevision)>, AutomationError> {
        // `git check-ref-format --branch` also rejects a leading dash and
        // `HEAD`; the reason carries Git's own text either way.
        let local_ref = format!("refs/heads/{branch}");
        if branch.starts_with('-')
            || branch == "HEAD"
            || !git2::Reference::is_valid_name(&local_ref)
        {
            return Err(AutomationError::Deferred(
                DeferredReason::EvidenceUnavailable(format!(
                    "git check-ref-format --branch {branch}: fatal: '{branch}' is not a valid branch name"
                )),
            ));
        }
        let Ok(repo) = git2::Repository::open(self.root) else {
            return Ok(None);
        };
        let local = repo
            .find_reference(&local_ref)
            .and_then(|reference| reference.peel_to_commit())
            .map_err(|error| {
                AutomationError::Deferred(DeferredReason::EvidenceUnavailable(format!(
                    "git rev-parse --verify --end-of-options {local_ref}^{{commit}}: {}",
                    unresolved_text(&error)
                )))
            })?;
        let config = repo.config().map_err(unavailable)?;
        let remote = match config.get_string("remote.origin.url") {
            Ok(remote) => remote.trim().to_owned(),
            Err(error) if error.code() == git2::ErrorCode::NotFound => String::new(),
            Err(error) => return Err(unavailable(error)),
        };
        let normalized = remote.trim_end_matches(".git");
        let repository = normalized
            .strip_prefix("https://github.com/")
            .or_else(|| normalized.strip_prefix("git@github.com:"))
            .map(str::to_owned)
            .unwrap_or_else(|| {
                // `git rev-parse --path-format=absolute --git-common-dir`
                // prints the canonical path without libgit2's trailing slash.
                let common = repo.commondir().to_string_lossy();
                let identity = if remote.is_empty() {
                    common.trim_end_matches('/')
                } else {
                    &remote
                };
                format!("git:{}", digest(identity.as_bytes()))
            });
        // Preserve preparation's local-ahead rule, but never refresh origin.
        // Missing/unreadable tracking refs fall back to local, as before.
        let head = if preparation && !remote.is_empty() {
            repo.find_reference(&format!("refs/remotes/origin/{branch}"))
                .and_then(|reference| reference.peel_to_commit())
                .ok()
                .filter(|origin| {
                    origin.id() != local.id()
                        && !repo
                            .graph_descendant_of(local.id(), origin.id())
                            .unwrap_or(false)
                })
                .unwrap_or(local)
        } else {
            local
        };
        Ok(Some((
            repository,
            SourceRevision {
                commit: head.id().to_string(),
                tree: head.tree_id().to_string(),
            },
        )))
    }
}

/// Git's first stderr line when `rev-parse --verify` cannot name a commit: a
/// missing ref, or one that does not peel to a commit. Other failures keep
/// libgit2's own text.
fn unresolved_text(error: &git2::Error) -> String {
    match error.code() {
        git2::ErrorCode::NotFound | git2::ErrorCode::InvalidSpec | git2::ErrorCode::Peel => {
            "fatal: Needed a single revision".into()
        }
        _ => error.message().to_owned(),
    }
}

fn unavailable(error: git2::Error) -> AutomationError {
    AutomationError::Deferred(DeferredReason::EvidenceUnavailable(format!(
        "local Git inspection: {error}"
    )))
}
