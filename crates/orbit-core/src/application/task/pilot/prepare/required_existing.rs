//! Deterministic extraction of required existing change targets during
//! task-pilot preparation.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::fs::selector::{
    anchor_path, canonical_selector, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_engine::DispatchError;
use orbit_types::task::Task;

use super::super::assessment::validate_selector_target_kind;
use super::super::source::{GitPathKind, SourceSnapshot};
use super::run::NO_DIFF_TAGS;

/// Cues that mark a clause as read-only, optional, negated, or historical
/// rather than a required change target of this task.
const EXCLUSION_CUES: [&str; 39] = [
    "read-only",
    "read only",
    "readonly",
    "reference",
    "consult",
    "consulted",
    "consulting",
    "evidence",
    "background",
    "context only",
    "inspected only",
    "merely read",
    "only read",
    "only consulted",
    "incidental",
    "optional",
    "if needed",
    "may need",
    "might need",
    "for example",
    "e.g.",
    "such as",
    "do not edit",
    "do not modify",
    "do not delete",
    "do not remove",
    "do not change",
    "do not touch",
    "do not update",
    "don't edit",
    "don't modify",
    "don't delete",
    "don't change",
    "don't touch",
    "must not edit",
    "must not modify",
    "must not delete",
    "must not change",
    "never edit",
];

const EXTRA_EXCLUSION_CUES: [&str; 17] = [
    "never modify",
    "never delete",
    "never change",
    "leave unchanged",
    "keep unchanged",
    "unchanged",
    "without changing",
    "without editing",
    "without modifying",
    "without updating",
    "without deleting",
    "without touching",
    "introduced by",
    "caused by",
    "regression from",
    "prior edit",
    "previously ",
];

/// Clause prefixes that introduce an omitted/contrasted item inside a sentence.
const CONTRAST_SPLITS: [&str; 9] = [
    ", while ",
    ", except ",
    ", rather than ",
    ", instead of ",
    ", without ",
    ", omitting ",
    ", excluding ",
    " while keeping ",
    " instead of ",
];

const OMIT_PREFIX_CUES: [&str; 6] = [
    "while ",
    "except ",
    "rather than ",
    "instead of ",
    "omitting ",
    "excluding ",
];

/// Word-bounded action cues establishing that a named existing target must be
/// changed, deleted, migrated, or regenerated.
const CHANGE_ACTION_CUES: [&str; 53] = [
    "delete",
    "deleted",
    "deletes",
    "deleting",
    "deletion",
    "remove",
    "removed",
    "removes",
    "removing",
    "removal",
    "retire",
    "retired",
    "retires",
    "retiring",
    "edit",
    "edited",
    "edits",
    "editing",
    "modify",
    "modified",
    "modifies",
    "modifying",
    "modification",
    "update",
    "updated",
    "updates",
    "updating",
    "rewrite",
    "rewritten",
    "rewrites",
    "rewriting",
    "migrate",
    "migrated",
    "migrates",
    "migrating",
    "regenerate",
    "regenerated",
    "regenerates",
    "regenerating",
    "regeneration",
    "replace",
    "replaced",
    "replaces",
    "replacing",
    "rename",
    "renamed",
    "renames",
    "renaming",
    "change",
    "changed",
    "changes",
    "changing",
    "wire",
];

const EXTRA_CHANGE_ACTION_CUES: [&str; 7] = [
    "wired", "wires", "wiring", "move", "moved", "moves", "moving",
];

struct ExistingCandidate {
    canonical: String,
    anchor: String,
    basename: Option<String>,
    shares_creation_parent: bool,
}

/// Existing selectors in `task.context_files` (outside `creation_selectors`)
/// that resolve at the pinned source (or workspace) and that `task`'s
/// description or acceptance criteria explicitly establish as required changes.
pub(super) fn required_existing_selectors(
    action: &str,
    task: &Task,
    creation_selectors: &[String],
    workspace_root: &Path,
    source: Option<&SourceSnapshot>,
) -> Result<Vec<String>, DispatchError> {
    if task
        .tags
        .iter()
        .any(|tag| NO_DIFF_TAGS.contains(&tag.as_str()))
    {
        return Ok(Vec::new());
    }

    let creation_set = creation_selectors
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let creation_parents = creation_selectors
        .iter()
        .filter_map(|selector| anchor_path(selector).ok())
        .filter_map(|anchor| anchor.parent().map(|parent| parent.to_path_buf()))
        .collect::<BTreeSet<_>>();

    let mut seen_candidates = BTreeSet::new();
    let mut candidates = Vec::new();
    for raw in &task.context_files {
        let canonical = if source.is_some() {
            canonical_selector(raw)
        } else {
            canonical_selector_in_workspace(raw, workspace_root)
        };
        let Ok(canonical) = canonical else {
            continue;
        };
        if creation_set.contains(canonical.as_str()) || !seen_candidates.insert(canonical.clone()) {
            continue;
        }
        let Ok(anchor) = anchor_path(&canonical) else {
            continue;
        };
        let resolves = match source {
            Some(source) => {
                let expected_dir = canonical.starts_with("dir:");
                matches!(
                    (
                        source.path_kind(action, workspace_root, &anchor)?,
                        expected_dir
                    ),
                    (GitPathKind::Tree, true) | (GitPathKind::Blob, false)
                )
            }
            None => {
                exists_in_workspace(&canonical, workspace_root)
                    && validate_selector_target_kind(action, &task.id, &canonical, workspace_root)
                        .is_ok()
            }
        };
        if !resolves {
            continue;
        }
        let anchor_str = anchor.to_string_lossy().replace('\\', "/");
        let shares_creation_parent = !canonical.starts_with("dir:")
            && anchor
                .parent()
                .is_some_and(|parent| creation_parents.contains(parent));
        let basename = (!canonical.starts_with("dir:"))
            .then(|| {
                anchor
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            })
            .flatten();
        candidates.push(ExistingCandidate {
            canonical,
            anchor: anchor_str,
            basename,
            shares_creation_parent,
        });
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let clauses = specification_clauses(task);
    let mut required = BTreeSet::new();
    for clause in clauses {
        if !is_positive_change_clause(&clause) {
            continue;
        }
        for candidate in &candidates {
            if clause_names_candidate(&clause, candidate) {
                required.insert(candidate.canonical.clone());
            }
        }
    }
    Ok(required.into_iter().collect())
}

fn specification_clauses(task: &Task) -> Vec<String> {
    let mut clauses = Vec::new();
    for text in std::iter::once(task.description.as_str())
        .chain(task.acceptance_criteria.iter().map(String::as_str))
    {
        for line in text.lines() {
            let trimmed = strip_markdown_prefix(line);
            if trimmed.is_empty() {
                continue;
            }
            for sentence in split_sentences(trimmed) {
                for sub_clause in split_contrast_clauses(sentence) {
                    let sub_clause = sub_clause.trim();
                    if !sub_clause.is_empty() {
                        clauses.push(sub_clause.to_string());
                    }
                }
            }
        }
    }
    clauses
}

fn strip_markdown_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix('>') {
        rest = after.trim_start();
    }
    if let Some(after) = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))
        .or_else(|| rest.strip_prefix("+ "))
    {
        return after.trim();
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && let Some(after) = rest[digits..].strip_prefix(". ")
    {
        return after.trim();
    }
    rest.trim()
}

fn split_sentences(text: &str) -> Vec<&str> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let is_boundary = match byte {
            b';' | b'!' | b'?' => true,
            b'.' => {
                let next_is_break = index + 1 == bytes.len()
                    || bytes[index + 1].is_ascii_whitespace()
                    || matches!(bytes[index + 1], b')' | b']' | b'"' | b'\'' | b'`');
                next_is_break && !ends_abbreviation(&text[start..=index])
            }
            _ => false,
        };
        if is_boundary {
            let piece = text[start..index].trim();
            if !piece.is_empty() {
                sentences.push(piece);
            }
            start = index + 1;
        }
        index += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail);
    }
    sentences
}

fn ends_abbreviation(prefix: &str) -> bool {
    let lower = prefix.to_ascii_lowercase();
    lower.ends_with("e.g.") || lower.ends_with("i.e.") || lower.ends_with("vs.")
}

fn split_contrast_clauses(sentence: &str) -> Vec<&str> {
    let lower = sentence.to_ascii_lowercase();
    let mut cuts = Vec::new();
    for marker in CONTRAST_SPLITS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(marker) {
            let cut = from + rel + usize::from(marker.starts_with(','));
            cuts.push(cut);
            from += rel + marker.len();
        }
    }
    if cuts.is_empty() {
        return vec![sentence];
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut parts = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0;
    for cut in cuts {
        if cut > start {
            parts.push(sentence[start..cut].trim());
            start = cut;
        }
    }
    if start < sentence.len() {
        parts.push(sentence[start..].trim());
    }
    parts
}

fn is_positive_change_clause(clause: &str) -> bool {
    let lower = clause.to_ascii_lowercase();
    let trimmed = lower.trim_start();
    if OMIT_PREFIX_CUES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return false;
    }
    if EXCLUSION_CUES
        .iter()
        .chain(EXTRA_EXCLUSION_CUES.iter())
        .any(|cue| contains_cue(&lower, cue))
    {
        return false;
    }
    CHANGE_ACTION_CUES
        .iter()
        .chain(EXTRA_CHANGE_ACTION_CUES.iter())
        .any(|cue| contains_cue(&lower, cue))
}

fn contains_cue(haystack: &str, cue: &str) -> bool {
    if cue.ends_with(' ') {
        return haystack.contains(cue);
    }
    let bytes = haystack.as_bytes();
    haystack.match_indices(cue).any(|(at, matched)| {
        let before = at == 0 || !is_cue_word_byte(bytes[at - 1]);
        let end = at + matched.len();
        let after = end >= bytes.len()
            || !is_cue_word_byte(bytes[end])
            || (bytes[end] == b'.'
                && (end + 1 == bytes.len() || bytes[end + 1].is_ascii_whitespace()));
        before && after
    })
}

fn is_cue_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'/' | b'.')
}

fn clause_names_candidate(clause: &str, candidate: &ExistingCandidate) -> bool {
    if contains_path_token(clause, &candidate.canonical)
        || contains_path_token(clause, &candidate.anchor)
    {
        return true;
    }
    candidate.shares_creation_parent
        && candidate
            .basename
            .as_deref()
            .is_some_and(|basename| contains_path_token(clause, basename))
}

fn contains_path_token(haystack: &str, token: &str) -> bool {
    haystack.match_indices(token).any(|(at, matched)| {
        let before = at == 0 || is_path_boundary_before(haystack.as_bytes()[at - 1]);
        let end = at + matched.len();
        let after = end == haystack.len() || is_path_boundary_after(haystack.as_bytes(), end);
        before && after
    })
}

fn is_path_boundary_before(byte: u8) -> bool {
    !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'-'))
}

fn is_path_boundary_after(bytes: &[u8], end: usize) -> bool {
    let next = bytes[end];
    if next == b'.' {
        return end + 1 == bytes.len()
            || bytes[end + 1].is_ascii_whitespace()
            || matches!(bytes[end + 1], b')' | b']' | b'"' | b'\'' | b'`');
    }
    !(next.is_ascii_alphanumeric() || matches!(next, b'_' | b'/' | b'-'))
}
