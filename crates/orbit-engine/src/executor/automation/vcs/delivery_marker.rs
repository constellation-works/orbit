//! "Did this work actually reach that commit?", asked from local Git history.
//!
//! Every Orbit commit names its task ids in `Orbit-Task` trailers and, room
//! permitting, as bracketed subject markers (see `vcs::commit::message`); both
//! survive merge, squash and rebase.
//! So delivery is decided by "is a *message match* reachable from this commit",
//! never by "is a particular sha reachable" — a squash merge rewrites the sha
//! and keeps the marker.
//!
//! `worktree::dependency_delivery` asks the question about a dependency's
//! commits; `base::obsolescence` asks the same question about the commits a base
//! branch carries (ORB-10644). The rule lives here once so the two cannot
//! drift.

use std::path::Path;

use orbit_common::OrbitError;

use orbit_types::workflow::{Trailer, TrailerKey};

use super::git::git_output;

/// Task-id markers are short, bracketed, whitespace-free, and mix letters with
/// digits (`[ORB-10644]`, `[T20260430-31B]`, `[GITHUB-PR-902]`). The bound
/// keeps an unrelated bracketed prose fragment out of the marker set.
const MAX_MARKER_LEN: usize = 64;

/// Commits whose message names `marker` (brackets included) within `scope`:
/// as a bracketed marker, as one id of an older comma-separated bracket list
/// (`[ORB-1, ORB-2]`), or as an `Orbit-Task` trailer value.
pub(super) fn commits_matching(
    repo_root: &Path,
    marker: &str,
    scope: &[&str],
) -> Result<Vec<String>, OrbitError> {
    let id = marker
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(marker);
    let id = escape_extended_regex(id);
    // Anchor the comma-list alternative inside brackets: an id is a list
    // member only between `[` and the matching `]`, with no other `[` in
    // between (each `[` opens its own candidate, as in `delivery_markers`),
    // never in prose such as `Related work: ORB-111, ORB-222, ORB-333`.
    let grep = format!("--grep=\\[([^][]*, ?)?{id}(, ?[^][]*)?\\]|^Orbit-Task: {id}$");
    let mut args = vec![
        "log",
        "--no-color",
        "--format=%H",
        "--extended-regexp",
        &grep,
    ];
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
///
/// A `Fixes:` trailer quotes the subject of the commit a repair fixes, so any
/// marker inside it names someone else's delivery. Trailer lines after the
/// subject are skipped; only the commit's own subject and body carry ownership.
///
/// An `Orbit-Task: <id>` trailer yields `[<id>]`, so a bundle whose subject
/// had no room for every id still reports each one. An older subject's
/// comma-separated list (`[ORB-1, ORB-2]`) yields one marker per id.
pub(super) fn delivery_markers(message: &str) -> Vec<String> {
    let owned = owned_text(message);
    let mut markers: Vec<String> = Vec::new();
    let mut push = |token: &str| {
        if is_marker(token) {
            let marker = format!("[{token}]");
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }
    };
    for candidate in owned.split('[').skip(1) {
        let Some((token, _)) = candidate.split_once(']') else {
            continue;
        };
        if token.contains(',') {
            token.split(',').map(str::trim).for_each(&mut push);
        } else {
            push(token);
        }
    }
    for line in owned.lines().skip(1) {
        if let Some(trailer) = Trailer::parse_line(line)
            && trailer.key == TrailerKey::OrbitTask
        {
            push(trailer.value.trim());
        }
    }
    markers
}

/// `text` with every POSIX extended-regex metacharacter escaped.
fn escape_extended_regex(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if r"\.[]()*+?{}|^$".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// `message` without its quoted-attribution trailers (`Fixes: <sha> ("<subject>")`).
/// The first line is the subject and is always kept.
fn owned_text(message: &str) -> String {
    message
        .split('\n')
        .enumerate()
        .filter(|(index, line)| *index == 0 || !line.trim_start().starts_with("Fixes:"))
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
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
