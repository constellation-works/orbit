use std::path::Path;

use orbit_common::OrbitError;

use super::super::git::{git_failure_error, git_run, git_success, git_timeout_error};
use super::merge::{checkout_holding_branch, ensure_base_checkout_ready};

/// Merge the commit a moved remote published on `base` into the local `base`
/// branch, in the checkout that holds it.
///
/// The merge only ever adds history: local commits (and the merge shas the
/// run recorded for its tasks) keep their ids and stay reachable from the new
/// tip, which is what lets a local delivery publish after the remote moved
/// without rewriting anything. When the local base is already an ancestor of
/// the remote commit the merge is a plain fast-forward. A conflicting remote
/// is not resolved here: the merge is aborted so the base is left as it was,
/// and the typed error names why the base was not published.
pub(in crate::executor::automation::vcs) fn merge_remote_head_into_base(
    repo_root: &Path,
    base: &str,
    remote_sha: &str,
) -> Result<(), OrbitError> {
    let holder = checkout_holding_branch(repo_root, base)?;
    let checkout = holder.clone().unwrap_or_else(|| repo_root.to_path_buf());
    if holder.is_none() {
        git_success(&checkout, &["checkout", base])?;
    }
    ensure_base_checkout_ready(&checkout, Some(remote_sha))?;
    let message = format!(
        "Merge remote '{base}' at {remote_sha} into local '{base}'\n\n\
         Integrates commits other hosts published before this host pushed its own."
    );
    let args = ["merge", "--no-edit", "-m", message.as_str(), remote_sha];
    let outcome = git_run(&checkout, &args)?;
    if outcome.success {
        return Ok(());
    }
    let _ = git_success(&checkout, &["merge", "--abort"]);
    if outcome.timed_out {
        return Err(git_timeout_error(
            &checkout,
            &args,
            outcome.timeout_ms,
            &outcome.stderr,
        ));
    }
    let failure = git_failure_error(&checkout, &args, outcome.exit_code, &outcome.stderr);
    Err(OrbitError::Execution(format!(
        "push_integration_failed: could not merge remote '{base}' ({remote_sha}) into the local \
         base, so it is not published and the delivery stays in review: {failure}"
    )))
}
