//! Prerequisite reads a completion poll would repeat without learning
//! anything new [ORB-25864].
//!
//! Every pass of a completion poll takes a workflow prerequisite snapshot,
//! and each pipeline polls its own worktree, all from the automation
//! account's shared REST quota. Two reads are kept per worktree instead of
//! repeated:
//!
//! - The repository identity (id, full name and API URL) every snapshot pins
//!   its evidence to, for [`IDENTITY_TTL`]. A snapshot that finds its
//!   evidence unreadable forgets it, so a renamed or replaced repository is
//!   read again before the snapshot retries.
//! - A file's contents at a full commit SHA, which that commit fixes for
//!   good. At most [`MAX_PINNED_READS`] are kept; past that the cache starts
//!   over.

use orbit_common::OrbitError;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::super::completion_deadline::Deadline;

/// How long one worktree's repository identity is reused.
const IDENTITY_TTL: Duration = Duration::from_secs(10 * 60);
/// Commit-pinned reads kept across every worktree.
const MAX_PINNED_READS: usize = 256;

type Identities = Mutex<HashMap<PathBuf, (Instant, Value)>>;
type PinnedReads = Mutex<HashMap<(PathBuf, String), Value>>;

fn identities() -> &'static Identities {
    static IDENTITIES: OnceLock<Identities> = OnceLock::new();
    IDENTITIES.get_or_init(Identities::default)
}

fn pinned_reads() -> &'static PinnedReads {
    static PINNED: OnceLock<PinnedReads> = OnceLock::new();
    PINNED.get_or_init(PinnedReads::default)
}

/// The repository `workspace`'s `gh` resolves, as `id`, `full_name` and
/// `url`: reused while fresh, otherwise read once and kept.
pub(super) fn repository_identity(
    deadline: Deadline,
    workspace: &Path,
) -> Result<Value, OrbitError> {
    if let Ok(cached) = identities().lock()
        && let Some((read_at, identity)) = cached.get(workspace)
        && read_at.elapsed() < IDENTITY_TTL
    {
        return Ok(identity.clone());
    }
    let repo = super::prerequisite::api(deadline, workspace, "repos/{owner}/{repo}")?;
    let identity = json!({"id":repo["id"],"full_name":repo["full_name"],"url":repo["url"]});
    if let Ok(mut cached) = identities().lock() {
        cached.insert(workspace.to_path_buf(), (Instant::now(), identity.clone()));
    }
    Ok(identity)
}

/// Read `workspace`'s repository identity afresh next time.
pub(super) fn forget_repository_identity(workspace: &Path) {
    if let Ok(mut cached) = identities().lock() {
        cached.remove(workspace);
    }
}

/// `path`'s contents at `commit`. A full commit SHA fixes them, so they are
/// read once per worktree; any other `commit` is read every time.
pub(super) fn contents_at_commit(
    deadline: Deadline,
    workspace: &Path,
    path: &str,
    commit: &str,
) -> Result<Value, OrbitError> {
    let endpoint = format!("repos/{{owner}}/{{repo}}/contents/{path}?ref={commit}");
    if !super::input::valid_expected_remote_sha(Some(commit)) {
        return super::prerequisite::api(deadline, workspace, &endpoint);
    }
    let key = (workspace.to_path_buf(), endpoint);
    if let Ok(cached) = pinned_reads().lock()
        && let Some(contents) = cached.get(&key)
    {
        return Ok(contents.clone());
    }
    let contents = super::prerequisite::api(deadline, workspace, &key.1)?;
    if let Ok(mut cached) = pinned_reads().lock() {
        if cached.len() >= MAX_PINNED_READS {
            cached.clear();
        }
        cached.insert(key, contents.clone());
    }
    Ok(contents)
}
