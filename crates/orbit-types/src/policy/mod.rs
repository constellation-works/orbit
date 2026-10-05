//! Domain contracts for this Orbit types module.

mod error;
mod fs_rules;
mod glob;
mod policy_decision;
mod policy_def;
mod role;
pub use error::PolicyError;
pub use fs_rules::CompiledFsRules;

pub use glob::{
    GlobReach, compile_glob_regex, join_normal_components, match_glob, normalize_glob_path,
};
pub use policy_decision::PolicyDecision;
pub use policy_def::{
    DEFAULT_POLICY_NAME, FsCheckResult, FsOperation, FsProfile, PolicyDef, ResolvedFsProfile,
    UNRESTRICTED_FS_PROFILE,
};
pub use role::Role;

#[cfg(test)]
mod tests;
