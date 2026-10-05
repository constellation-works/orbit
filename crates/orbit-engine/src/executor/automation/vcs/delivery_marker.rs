//! "Did this work actually reach that commit?", asked from local Git history.
//!
//! Every Orbit commit subject carries the bracketed task id (see
//! `vcs::commit::message`), and the marker survives merge, squash and rebase.
//! So delivery is decided by "is a *message match* reachable from this commit",
//! never by "is a particular sha reachable" — a squash merge rewrites the sha
//! and keeps the marker.
//!
//! `worktree::dependency_delivery` asks the question about a dependency's
//! commits; `base_obsolescence` asks the same question about the commits a base
//! branch carries (ORB-10644). The rule lives here once so the two cannot
//! drift.

use std::path::Path;

use orbit_common::OrbitError;

use super::git::git_output;

/// Task-id markers are short, bracketed, whitespace-free, and mix letters with
/// digits (`[ORB-10644]`, `[T20260430-31B]`, `[GITHUB-PR-902]`). The bound
/// keeps an unrelated bracketed prose fragment out of the marker set.
const MAX_MARKER_LEN: usize = 64;

/// Commits whose message contains `marker` (brackets included) within `scope`.
pub(super) fn commits_matching(
    repo_root: &Path,
    marker: &str,
    scope: &[&str],
) -> Result<Vec<String>, OrbitError> {
    let grep = format!("--grep={marker}");
    let mut args = vec!["log", "--no-color", "--format=%H", "--fixed-strings", &grep];
    args.extend_from_slice(scope);
    Ok(git_output(repo_root, &args)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Whether any commit reachable from `from` carries `marker`.
pub(super) fn marker_reachable(
    repo_root: &Path,
    marker: &str,
    from: &str,
) -> Result<bool, OrbitError> {
    Ok(!commits_matching(repo_root, marker, &["--max-count=1", from])?.is_empty())
}

/// The bracketed delivery markers a commit message carries, in first-seen
/// order and including their brackets so they can be grepped verbatim.
///
/// Deliberately conservative: a token that is not shaped like an id (no digit,
/// no letter, embedded whitespace, over-long) is dropped. Each opening bracket
/// starts a new candidate, so an unmatched bracket in prose or a truncated
/// title cannot hide a later delivery marker.
pub(super) fn delivery_markers(message: &str) -> Vec<String> {
    let mut markers: Vec<String> = Vec::new();
    for candidate in message.split('[').skip(1) {
        let Some((token, _)) = candidate.split_once(']') else {
            continue;
        };
        if is_marker(token) {
            let marker = format!("[{token}]");
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    }
    markers
}

fn is_marker(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_MARKER_LEN
        && !token.contains(char::is_whitespace)
        && !token.contains('[')
        && token.chars().any(|character| character.is_ascii_digit())
        && token
            .chars()
            .any(|character| character.is_ascii_alphabetic())
}
