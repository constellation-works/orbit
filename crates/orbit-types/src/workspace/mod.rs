//! Domain contracts for this Orbit types module.

mod error;
mod publication;
mod registry;
pub use error::WorkspaceError;

#[cfg(test)]
mod tests;

pub use publication::{
    DEFAULT_PUBLICATION_BRANCH, WorkspacePublicationBinding, canonicalize_publication_branch,
    git_remote_identity, git_remotes_equivalent, redact_git_remote, validate_git_commit_id,
    validate_last_success, validate_publication_branch, validate_publication_id,
    validate_publication_remote, validate_source_repository_fingerprint,
};
pub use registry::{
    DEFAULT_BASE_BRANCH, WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout,
    WorkspaceCheckoutRole, WorkspacePaths, WorkspaceRegistry, WorkspaceStatus,
};
