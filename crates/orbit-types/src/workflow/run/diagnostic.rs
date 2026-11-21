//! Operator context appended to CLI and orphaned-worker failures.

/// Separates a generic CLI exit failure from its untrusted stderr context.
pub const CLI_STDERR_TAIL_SEPARATOR: &str = "\nstderr tail:\n";

/// Separates a generic CLI exit from an unclassified provider error frame.
pub const CLI_PROVIDER_ERROR_SEPARATOR: &str = "\nprovider error:\n";

/// Diagnostic-only worker log provenance appended to an orphan failure.
pub const WORKER_PROVENANCE_SEPARATOR: &str = "\nworker provenance:\n";

/// The authoritative failure text, excluding generic CLI diagnostic context.
///
/// Wrappers may prefix a step or run name. Only generic CLI failures can
/// introduce this suffix: a bare exit or signal termination, or either form
/// followed by a provider terminal diagnostic [ORB-24697]. Provider-specific
/// diagnostics retain their existing classification. Stderr can quote denial
/// words, typed failure markers or the check-repair handoff without acquiring
/// control-plane meaning.
/// Orphan diagnostics likewise exclude their log-backed worker provenance:
/// a returned error there describes evidence, not a new settlement decision.
#[must_use]
pub fn failure_classification_text(message: &str) -> &str {
    if let Some((failure, _)) = message.split_once(WORKER_PROVENANCE_SEPARATOR)
        && failure.contains("worker exit:")
    {
        return failure;
    }
    let Some(boundary) = [CLI_STDERR_TAIL_SEPARATOR, CLI_PROVIDER_ERROR_SEPARATOR]
        .into_iter()
        .filter_map(|separator| message.find(separator))
        .min()
    else {
        return message;
    };
    let failure = &message[..boundary];
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
