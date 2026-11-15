//! Operator context appended to generic CLI failures.

/// Separates a generic CLI exit failure from its untrusted stderr context.
pub const CLI_STDERR_TAIL_SEPARATOR: &str = "\nstderr tail:\n";

/// The authoritative failure text, excluding a generic CLI stderr tail.
///
/// Wrappers may prefix a step or run name. Only generic CLI failures can
/// introduce this suffix: a bare exit or signal termination, or either form
/// followed by a provider terminal diagnostic [ORB-24697]. Provider-specific
/// diagnostics retain their existing classification. Stderr can quote denial
/// words, typed failure markers or the check-repair handoff without acquiring
/// control-plane meaning.
#[must_use]
pub fn failure_classification_text(message: &str) -> &str {
    let Some((failure, _)) = message.split_once(CLI_STDERR_TAIL_SEPARATOR) else {
        return message;
    };
    let generic_exit = failure
        .rsplit_once("cli subprocess exited with code ")
        .is_some_and(|(_, code)| {
            code.split_whitespace()
                .next()
                .is_some_and(|code| code.parse::<i32>().is_ok())
        });
    let generic_signal = failure
        .rsplit_once("cli subprocess was terminated by a signal and reported no exit code")
        .is_some_and(|(_, diagnostic)| diagnostic.is_empty() || diagnostic.starts_with(' '));
    if generic_exit || generic_signal {
        failure
    } else {
        message
    }
}
