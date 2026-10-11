//! Pass 5 [ORB-15434]: the deterministic cause an incident shares with the
//! same failure reported at other surfaces or by other actors.
//!
//! The signature keys an incident by its reporting surface and actor, so one
//! failed step that a parent's wait, a worker and a guard each record reads as
//! three problems. The cause looks past the reporter to the structured
//! evidence the root's message and columns carry, by shape alone:
//!
//! - a **step**: the last backtick-quoted name introduced by `action`, `step`
//!   or `activity` on the message's first line (the innermost of a nested
//!   chain such as ``action `a` failed: … action `b` failed``), else the
//!   row's `activity_id` column;
//! - a **code**: on the first line after that step, the first `kind=`,
//!   `code=`, `error.code=` or `error_code=` value, else the first
//!   `[compound_tag]` that opens a message segment.
//!
//! Incidents of one class share a cause when they name the same step and the
//! same code, or the same step and the same normalized first line after it
//! (later lines are usually captured output, which differs run to run). Two
//! codes under one step are two causes: the code is the producer's own
//! statement of which failure it was, so its distinct values stay distinct
//! problems. With neither a step nor a code the cause is the incident's own
//! signature, so nothing is merged without evidence.

use super::signature::{fnv1a, normalize_message};
use super::types::{CauseBasis, FailureClass, IncidentCause};

/// Words that introduce a backtick-quoted step or action name.
const STEP_KEYWORDS: &[&str] = &["action", "step", "activity"];

/// Keys whose `key=value` value is a structured failure code.
const CODE_KEYS: &[&str] = &["kind", "code", "error.code", "error_code"];

/// Longest step name or code accepted. Longer runs are prose, not identifiers.
const MAX_IDENTIFIER_CHARS: usize = 96;

/// The root evidence a cause is derived from.
pub(super) struct CauseInput<'a> {
    pub(super) class: FailureClass,
    pub(super) signature: &'a str,
    pub(super) surface: &'a str,
    pub(super) activity_id: Option<&'a str>,
    pub(super) message: Option<&'a str>,
}

struct Step<'a> {
    keyword: &'a str,
    name: &'a str,
    /// The message after the step's closing backtick, or all of it when the
    /// step came from the `activity_id` column. Only its first line is read.
    rest: &'a str,
    from_column: bool,
}

struct Code {
    display: String,
    value: String,
}

/// Derives an incident's cause. Pure: the same root evidence always yields
/// the same key, so a refresh over the same events never regroups them.
pub(super) fn cause_for(input: &CauseInput<'_>) -> IncidentCause {
    let message = input.message.unwrap_or_default();
    let step = innermost_step(message).or_else(|| {
        let name = input
            .activity_id
            .map(str::trim)
            .filter(|name| is_identifier(name))?;
        Some(Step {
            keyword: "activity",
            name,
            rest: message,
            from_column: true,
        })
    });
    match step {
        Some(step) => {
            if let Some(code) = structured_code(first_line(step.rest)) {
                return cause(
                    input.class,
                    CauseBasis::StepCode,
                    &[step.name, &code.value],
                    format!("{} `{}` · {}", step.keyword, step.name, code.display),
                );
            }
            let tail = normalize_message(first_line(step.rest));
            // An activity column with no message is no more specific than
            // the signature it would replace.
            if !(step.from_column && tail.is_empty()) {
                return cause(
                    input.class,
                    CauseBasis::StepMessage,
                    &[step.name, &tail],
                    format!("{} `{}`", step.keyword, step.name),
                );
            }
        }
        None => {
            if let Some(code) = structured_code(first_line(message)) {
                return cause(input.class, CauseBasis::Code, &[&code.value], code.display);
            }
        }
    }
    cause(
        input.class,
        CauseBasis::Signature,
        &[input.signature],
        input.surface.to_string(),
    )
}

fn cause(class: FailureClass, basis: CauseBasis, parts: &[&str], label: String) -> IncidentCause {
    let mut material = format!("{}\u{1f}{}", class.as_str(), basis.as_str());
    for part in parts {
        material.push('\u{1f}');
        material.push_str(part);
    }
    IncidentCause {
        key: format!("cause-{:016x}", fnv1a(&material)),
        label,
        basis,
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// The last ``<keyword> `name` `` on the message's first line.
fn innermost_step(message: &str) -> Option<Step<'_>> {
    let line = first_line(message);
    let mut found = None;
    let mut search = 0;
    while let Some(open) = line[search..].find('`').map(|at| search + at) {
        let Some(close) = line[open + 1..].find('`').map(|at| open + 1 + at) else {
            break;
        };
        let name = &line[open + 1..close];
        if is_identifier(name)
            && let Some(keyword) = keyword_before(&line[..open])
        {
            found = Some(Step {
                keyword,
                name,
                rest: &message[close + 1..],
                from_column: false,
            });
        }
        search = close + 1;
    }
    found
}

/// The step keyword ending `prefix`, followed by one space and standing as a
/// whole word.
fn keyword_before(prefix: &str) -> Option<&'static str> {
    let prefix = prefix.strip_suffix(' ')?;
    STEP_KEYWORDS.iter().copied().find(|keyword| {
        prefix.strip_suffix(keyword).is_some_and(|before| {
            !before
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= MAX_IDENTIFIER_CHARS
        && text.chars().any(|c| c.is_ascii_alphabetic())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

/// The first `key=value` code in `scope`, else the first `[compound_tag]`
/// that opens a segment (the scope's start, or after a token ending in `:`).
/// A tag must contain `_` or `.`, so a log level such as `[error]` is not a
/// code.
fn structured_code(scope: &str) -> Option<Code> {
    let mut tag = None;
    let mut segment_start = true;
    let tokens = scope.split(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | '(' | ')'));
    for token in tokens.filter(|token| !token.is_empty()) {
        if let Some((key, value)) = token.split_once('=')
            && CODE_KEYS.contains(&key)
        {
            let value = value.trim_matches(['"', '\'', '`', '.', ':']);
            if is_identifier(value) {
                return Some(Code {
                    display: format!("{key}={value}"),
                    value: value.to_string(),
                });
            }
        }
        if tag.is_none()
            && segment_start
            && let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']'))
            && is_identifier(inner)
            && inner.contains(['_', '.'])
        {
            tag = Some(Code {
                display: format!("[{inner}]"),
                value: inner.to_string(),
            });
        }
        segment_start = token.ends_with(':');
    }
    tag
}
