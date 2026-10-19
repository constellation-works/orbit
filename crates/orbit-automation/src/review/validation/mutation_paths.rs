//! Repository paths used to bind controls and repairs to the candidate.

use std::collections::BTreeSet;

use orbit_common::fs::selector::overlaps;
use orbit_types::workflow::{FindingDisposition, ReviewFinding, ReviewValidation};

use super::defects::ValidationDefect;

/// [ORB-14616] Every file a record says it temporarily mutated, as the
/// repository-relative path settlement compares between the reviewed and the
/// final candidate, paired with the record's command. Only the repository can
/// tell whether a target came back byte-identical, so settlement makes that
/// comparison itself; this only reads the targets. [ORB-14632] A target that
/// is not a repository-relative file path (absolute, home-relative, climbing
/// out of the repository or another selector kind) could never be compared,
/// so it is refused as [`ValidationDefect::MutationTargetInvalid`] rather than
/// passed. A `file:` selector reads as its path; `dir:` and `symbol:` are no
/// file and are refused. [ORB-16447] Each target carries the repaired blob
/// its record declares under any spelling of the same path.
pub fn mutation_targets(
    records: &[ReviewValidation],
) -> Result<Vec<MutationTarget<'_>>, ValidationDefect> {
    let mut targets = Vec::new();
    for record in records {
        for target in record
            .mutation_target
            .iter()
            .map(|target| target.trim())
            .filter(|target| !target.is_empty())
        {
            let path =
                repository_path(target).ok_or_else(|| ValidationDefect::MutationTargetInvalid {
                    command: record.command.clone(),
                    target: target.to_string(),
                })?;
            let repaired_blob = record
                .repaired_blob
                .iter()
                .find(|(declared, _)| repository_path(declared.trim()).as_ref() == Some(&path))
                .map(|(_, blob)| blob.trim());
            targets.push(MutationTarget {
                command: record.command.as_str(),
                path,
                repaired_blob,
            });
        }
    }
    Ok(targets)
}

/// One file a control says it temporarily mutated ([`mutation_targets`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationTarget<'a> {
    /// The command of the record that names it.
    pub command: &'a str,
    /// The target as a normalized repository-relative path.
    pub path: String,
    /// [ORB-16447] The blob id the record declares for the target's
    /// repaired state, if any.
    pub repaired_blob: Option<&'a str>,
}

/// [ORB-16447] Every path a `repaired` finding lists, as a normalized
/// repository-relative path: the files the reviewer says its own repair
/// changed. A path that names no single file is left out.
pub fn repaired_paths(findings: &[ReviewFinding]) -> BTreeSet<String> {
    findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Repaired)
        .flat_map(|finding| &finding.paths)
        .filter_map(|path| repository_path(path.trim()))
        .collect()
}

/// `target` as a normalized repository-relative path, or `None` when it
/// names nothing a candidate's tree could hold.
fn repository_path(target: &str) -> Option<String> {
    let path = target.strip_prefix("file:").unwrap_or(target);
    let drive = matches!(path.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    if drive
        || ["dir:", "symbol:"]
            .iter()
            .any(|kind| path.starts_with(kind))
        || path.starts_with(['/', '~'])
        || path.contains(['\\', '\0'])
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            // Git's own store is no file of the candidate.
            part if part.eq_ignore_ascii_case(".git") => return None,
            part => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Whether `source` overlaps any scope selector. A bare path reads as the
/// `file:` selector of that path.
pub fn in_scope(source: &str, scope: &[String]) -> bool {
    let source = if ["file:", "dir:", "symbol:"]
        .iter()
        .any(|kind| source.starts_with(kind))
    {
        source.to_string()
    } else {
        format!("file:{}", source.trim_start_matches("./"))
    };
    scope.iter().any(|selector| overlaps(selector, &source))
}
