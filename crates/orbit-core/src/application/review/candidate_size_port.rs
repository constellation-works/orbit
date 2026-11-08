//! Candidate sizing consumed by reviewer effort selection.

use std::path::Path;

/// Read candidate size without depending on the codebase analytics owner.
pub(in crate::application) trait ReviewCandidateSizePort {
    /// Return reviewable lines and changed files, or `None` when the repository
    /// or either commit cannot be read.
    fn review_candidate_size(&self, repo_path: &Path, base: &str, head: &str)
    -> Option<(u64, u64)>;
}
