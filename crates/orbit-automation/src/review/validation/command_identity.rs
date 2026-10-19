//! Effective check identity and host-command normalization.

use orbit_types::workflow::ReviewValidation;

/// One check: both records carry the same record id, or they share an
/// effective identity.
///
/// Without a shared record id, identical commands are not enough when either
/// record has a non-empty `check`: that identity takes precedence. For example,
/// a superseded `cargo test -p x` with `check: "unit"` is not replaced by a
/// required pass of `cargo test -p x` that omits `check`. The replacement must
/// carry `check: "unit"` or normalize to the command `unit`. Conversely, an
/// explicit `check: "cargo test -p x"` matches that command without a check.
/// This rule also governs retained obligations and diagnostic contradictions.
pub(super) fn same_check(left: &ReviewValidation, right: &ReviewValidation) -> bool {
    if matches!(
        (left.record_id(), right.record_id()),
        (Some(left), Some(right)) if left == right
    ) {
        return true;
    }
    matches!(
        (effective_check_identity(left), effective_check_identity(right)),
        (Some(left), Some(right)) if left == right
    )
}

/// The explicit identity takes precedence over the normalized command.
fn effective_check_identity(record: &ReviewValidation) -> Option<String> {
    check_identity(record)
        .map(str::to_owned)
        .or_else(|| normalized_command(record))
}

/// A present, non-empty `check` identity.
fn check_identity(record: &ReviewValidation) -> Option<&str> {
    record
        .check
        .as_deref()
        .map(str::trim)
        .filter(|check| !check.is_empty())
}

/// Whether `record` is the host-required command.
///
/// Whitespace and leading POSIX environment assignments do not make a
/// different check. The command that remains is compared in full. A
/// reviewer may also set `check` to the host command string when the run
/// is wrapped in any other way (`env`, a shell prefix); that identity is
/// the host command.
pub fn same_host_command(record: &ReviewValidation, host_command: &str) -> bool {
    let same_command = matches!(
        (
            normalized_command(record),
            normalize_command_text(host_command),
        ),
        (Some(left), Some(right)) if left == right
    );
    let same_identity = check_identity(record).is_some_and(|check| check == host_command.trim());
    same_command || same_identity
}

/// The command with whitespace runs collapsed and leading POSIX environment
/// assignments removed, so `TMPDIR="$PWD/.orbit/tmp" make ci-fast` and
/// `make  ci-fast` are one check. `make ci-fast-extra` and `FOO=1 make other`
/// stay different checks.
fn normalized_command(record: &ReviewValidation) -> Option<String> {
    normalize_command_text(&record.command)
}

fn normalize_command_text(command: &str) -> Option<String> {
    let command = strip_leading_env_assignments(command)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!command.is_empty()).then_some(command)
}

/// Drop leading `NAME=value` words. A quoted value, including one with
/// spaces, stays part of the assignment word. The command text that remains
/// is returned unchanged so later whitespace collapsing matches the previous
/// comparison.
fn strip_leading_env_assignments(command: &str) -> &str {
    let mut rest = command;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return trimmed;
        }
        let (word, after) = first_shell_word(trimmed);
        if word.is_empty() || !is_env_assignment(word) {
            return trimmed;
        }
        rest = after;
    }
}

/// The first shell word and the text after it.
///
/// Quotes keep their contents, spaces included, in the word. A backslash
/// escapes the next character outside quotes and inside double quotes. The
/// raw word is returned, quotes included, because only assignment words are
/// discarded; the surviving command is collapsed separately.
fn first_shell_word(command: &str) -> (&str, &str) {
    let mut quote: Option<char> = None;
    let mut chars = command.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                if ch == '\\' {
                    chars.next();
                } else if ch == '"' {
                    quote = None;
                }
            }
            Some(_) => {}
            None if ch.is_whitespace() => {
                return (&command[..idx], &command[idx..]);
            }
            None => match ch {
                '\'' => quote = Some('\''),
                '"' => quote = Some('"'),
                '\\' => {
                    chars.next();
                }
                _ => {}
            },
        }
    }
    (command, "")
}

/// A POSIX assignment word: an unquoted `NAME` immediately followed by `=`.
///
/// `NAME` is `[A-Za-z_][A-Za-z0-9_]*`. The value may be quoted. A word whose
/// name is quoted, or that has no `=`, is a command word.
fn is_env_assignment(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_posix_name_start(first) {
        return false;
    }
    for ch in chars {
        if is_posix_name_continue(ch) {
            continue;
        }
        return ch == '=';
    }
    false
}

fn is_posix_name_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_posix_name_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}
