//! Apply the retained final-recovery edits an operator's source-repair
//! admission binds over the adopted candidate.
//!
//! The host rereads the bytes from the source's own checkout and checks them
//! against the bound digest; this side checks the digest again before Git
//! sees the bytes. The patch is applied with a three-way fallback onto the
//! squash-merged candidate, so the edits reach the successor as uncommitted
//! changes and its own commit step delivers them under the usual gates.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::SourceRepairRetainedPatch;

use crate::executor::automation::vcs::git::{git_output, git_run_bytes, timeout_recovery_error};

/// What applying the bound edits left in the checkout.
pub(super) enum RetainedApplied {
    /// The edits applied without conflict.
    Clean,
    /// The edits left conflict markers in `paths`.
    Conflict { paths: Vec<String>, output: String },
    /// Nothing usable was applied, and why.
    Refused(String),
}

/// Apply `bytes`, which must be exactly the `bound` patch, in `workspace`.
pub(super) fn apply_retained_patch(
    process_env: &orbit_common::Env,
    workspace: &Path,
    bound: &SourceRepairRetainedPatch,
    bytes: &[u8],
) -> Result<RetainedApplied, OrbitError> {
    if sha256_hex(bytes) != bound.sha256 || bytes.len() as u64 != bound.bytes {
        return Ok(RetainedApplied::Refused(
            "retained final-recovery edits do not match the bound digest".into(),
        ));
    }
    let applied = git_run_bytes(
        process_env,
        workspace,
        &["apply", "--3way", "--whitespace=nowarn", "-"],
        Some(bytes),
    )?;
    if applied.timed_out {
        return Err(timeout_recovery_error(
            applied.timeout_ms,
            format!(
                "retained edits: git apply timed out after {}ms",
                applied.timeout_ms
            ),
        ));
    }
    if applied.success {
        return Ok(RetainedApplied::Clean);
    }
    let unmerged = git_output(
        process_env,
        workspace,
        &["diff", "--name-only", "--diff-filter=U"],
    )?;
    let paths: Vec<String> = unmerged.lines().map(str::to_string).collect();
    if paths.is_empty() {
        return Ok(RetainedApplied::Refused(format!(
            "retained final-recovery edits do not apply to the candidate: {}",
            applied.stderr.trim()
        )));
    }
    Ok(RetainedApplied::Conflict {
        paths,
        output: applied.stderr,
    })
}
