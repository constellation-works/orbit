//! Admit a `required_os` entry whose criterion names no OS token only when
//! its evidence cites a repository guide rule that a task tag triggers.
//!
//! The guide is the workspace's own validation contract (for example a
//! `CLAUDE.md`/`AGENTS.md` line requiring a local CodeQL run for a task
//! tagged `security`, with only Linux able to extract every module). Apply
//! reads the cited lines from the pinned source: one cited line must name
//! CodeQL together with a tag the guide names as its trigger (a
//! backtick-quoted tag after `tagged`) that the task carries, and another
//! cited line in the same guide must state that only Linux can produce the
//! run. Any other word on the line is not a trigger, so a task carrying
//! `docs` or `github` is not locked by a line that merely mentions those
//! words. Any other citation stays with the caller's portable-criterion
//! refusal, so a platform mention alone never becomes an OS lock.

use std::path::Path;

use orbit_engine::DispatchError;
use orbit_types::task::HostOs;

use super::gate_source::{self, GateSource};
use super::source::SourceSnapshot;

/// Whether evidence cites a guide rule this check can admit.
pub(super) enum GuideGateDecision {
    /// Evidence does not name CodeQL, so it cites no guide validation rule.
    NotCited,
    /// The pinned guide confirms a tag-triggered Linux-only validation.
    Admit,
}

/// One `required_os` entry checked against the pinned guide.
pub(super) struct GuideGateCheck<'a> {
    pub(super) action: &'a str,
    pub(super) task_id: &'a str,
    pub(super) criterion_index: usize,
    pub(super) evidence: &'a str,
    pub(super) os: HostOs,
    pub(super) tags: &'a [String],
    pub(super) workspace_root: &'a Path,
    pub(super) source: Option<&'a SourceSnapshot>,
}

/// Guide files that carry a workspace's repository validation rules.
const GUIDE_FILES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// Admit `check`, leave it to the caller, or refuse a malformed citation.
pub(super) fn admit_guide_gate(
    check: &GuideGateCheck<'_>,
) -> Result<GuideGateDecision, DispatchError> {
    let source_check = GateSource {
        action: check.action,
        task_id: check.task_id,
        criterion_index: check.criterion_index,
        workspace_root: check.workspace_root,
        source: check.source,
    };
    if !check.evidence.to_ascii_lowercase().contains("codeql") {
        return Ok(GuideGateDecision::NotCited);
    }
    let mut citations = Vec::new();
    for citation in gate_source::path_line_citations(check.evidence) {
        if !citations.contains(&citation) {
            citations.push(citation);
        }
    }
    if citations.is_empty() || citations.iter().any(|(path, _)| !is_guide_path(path)) {
        return Err(gate_source::refuse(
            &source_check,
            "evidence naming CodeQL must cite the repository guide rule at \
             AGENTS.md:<line> or CLAUDE.md:<line> in the pinned source; leave \
             required_os empty for portable criteria",
        ));
    }
    if citations.iter().any(|(path, _)| path != &citations[0].0) {
        return Err(gate_source::refuse(
            &source_check,
            "evidence must cite one repository guide file for the CodeQL rule",
        ));
    }
    if check.os != HostOs::Linux {
        return Err(gate_source::refuse(
            &source_check,
            "a guide rule that only Linux can satisfy must name os linux; \
             leave required_os empty",
        ));
    }
    let (path, _) = &citations[0];
    let text =
        gate_source::read_pinned_source(&source_check, path, "a repository guide rule", "guide ")?;
    let lines: Vec<&str> = text.lines().collect();
    let mut cited = Vec::new();
    for (_, line_no) in &citations {
        let Some(line) = lines.get(line_no - 1) else {
            return Err(gate_source::refuse(
                &source_check,
                &format!("pinned guide has no line {line_no} at {path}"),
            ));
        };
        cited.push((*line_no, *line));
    }
    let trigger = cited.iter().any(|(_, line)| {
        line.to_ascii_lowercase().contains("codeql")
            && guide_trigger_tags(line).iter().any(|named_tag| {
                check
                    .tags
                    .iter()
                    .any(|tag| tag.trim().eq_ignore_ascii_case(named_tag))
            })
    });
    if !trigger {
        return Err(gate_source::refuse(
            &source_check,
            &format!(
                "no cited guide line at {path} names CodeQL together with a tag this task \
                 carries; leave required_os empty for portable criteria"
            ),
        ));
    }
    let only_linux = cited
        .iter()
        .any(|(_, line)| line.to_ascii_lowercase().contains("only linux"));
    if !only_linux {
        return Err(gate_source::refuse(
            &source_check,
            &format!(
                "no cited guide line at {path} states that only Linux can produce the \
                 required validation; leave required_os empty for portable criteria"
            ),
        ));
    }
    Ok(GuideGateDecision::Admit)
}

fn is_guide_path(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    GUIDE_FILES.contains(&file)
}

/// The tags a guide line names as CodeQL triggers, lowercased: the
/// backtick-quoted tags that follow the word `tagged`, joined only by `or`,
/// `and` or commas, as in "is tagged `security` or `security-review`, or
/// edits ...". Every other word on the line, such as `docs`, `github` or
/// `main`, names no trigger.
fn guide_trigger_tags(line: &str) -> Vec<String> {
    let lower = line.to_ascii_lowercase();
    let mut tags = Vec::new();
    for (index, _) in lower.match_indices("tagged") {
        let starts_word = index == 0 || !lower.as_bytes()[index - 1].is_ascii_alphanumeric();
        if !starts_word {
            continue;
        }
        for word in lower[index + "tagged".len()..].split_whitespace() {
            let word = word.trim_end_matches([',', '.', ';']);
            if matches!(word, "or" | "and") {
                continue;
            }
            match word
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
            {
                Some(tag) if !tag.is_empty() => tags.push(tag.to_string()),
                _ => break,
            }
        }
    }
    tags
}
