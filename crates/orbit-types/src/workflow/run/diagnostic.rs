//! Operator context appended to generic CLI failures.

/// Separates a generic CLI exit failure from its untrusted stderr context.
pub const CLI_STDERR_TAIL_SEPARATOR: &str = "\nstderr tail:\n";

/// The authoritative failure text, excluding a generic CLI stderr tail.
///
/// Wrappers may prefix a step or run name. Only the generic exit format can
/// introduce this suffix; provider-specific diagnostics retain their existing
/// classification. Stderr can quote denial words, typed failure markers or the
/// check-repair handoff without acquiring control-plane meaning.
#[must_use]
pub fn failure_classification_text(message: &str) -> &str {
    let Some((failure, _)) = message.split_once(CLI_STDERR_TAIL_SEPARATOR) else {
        return message;
    };
    let generic_exit = failure
        .rsplit_once("cli subprocess exited with code ")
        .is_some_and(|(_, code)| code.parse::<i32>().is_ok());
    if generic_exit
        || failure.ends_with("cli subprocess was terminated by a signal and reported no exit code")
    {
        failure
    } else {
        message
    }
}
