//! The short format a human-crew task body follows.
//!
//! Operator work is read by a person who wants to copy, run and verify, not to
//! study a brief. Filing checks the body and returns advisory warnings, so a
//! filer can repair it; nothing is refused, because internal and system filers
//! write human tasks too.

/// Longest human-task body, in characters, before filing warns. A summary, a
/// handful of steps and two or three command blocks fit well inside it; long
/// evidence belongs in a task artifact.
pub(crate) const HUMAN_TASK_BODY_MAX_CHARS: usize = 2000;

/// Warnings for a human-crew task body that departs from the short format:
/// a summary, numbered steps, fenced shell commands, and a `Verify` command.
pub(crate) fn human_task_format_warnings(description: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let mut has_step = false;
    let mut fenced_blocks = 0usize;
    let mut verify_block = false;
    let mut verify_pending = false;
    let mut in_fence = false;

    for line in description.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
            } else {
                in_fence = true;
                fenced_blocks += 1;
                if verify_pending {
                    verify_block = true;
                }
            }
            continue;
        }
        if in_fence {
            continue;
        }
        if is_numbered_step(trimmed) {
            has_step = true;
        }
        if is_verify_heading(trimmed) {
            verify_pending = true;
        }
    }

    if !has_step {
        warnings.push(
            "human-crew task has no numbered steps; write a 1-2 line summary, then `1.` `2.` steps"
                .to_string(),
        );
    }
    if fenced_blocks == 0 {
        warnings.push(
            "human-crew task has no fenced shell block; put each runnable command in a ```sh block the operator can copy"
                .to_string(),
        );
    }
    if !verify_block {
        warnings.push(
            "human-crew task has no verify command; end with a `Verify:` line followed by one fenced command"
                .to_string(),
        );
    }
    let chars = description.chars().count();
    if chars > HUMAN_TASK_BODY_MAX_CHARS {
        warnings.push(format!(
            "human-crew task body is {chars} characters, over the {HUMAN_TASK_BODY_MAX_CHARS} limit; keep it short and attach long evidence as a task artifact"
        ));
    }
    warnings
}

/// `1.` or `1)` followed by text.
fn is_numbered_step(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0
        && line[digits..]
            .strip_prefix(['.', ')'])
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

/// A line that opens the verify section: `Verify`, `Verify:`, `## Verify`,
/// `**Verify:**`.
fn is_verify_heading(line: &str) -> bool {
    let text = line.trim_start_matches(['#', '*', ' ']);
    text.get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("verify"))
}
