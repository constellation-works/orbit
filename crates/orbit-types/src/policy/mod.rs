//! Filesystem policy profile, glob matching, role, and policy-definition contracts; imports `resource` and `timestamp` within `orbit-types` only (I/O-free).

mod error;
mod fs_rules;
mod glob;
mod policy_def;
mod role;
mod timestamp;
pub use error::PolicyError;
pub use fs_rules::CompiledFsRules;

pub use glob::{
    GlobReach, compile_glob_regex, join_normal_components, match_glob, normalize_glob_path,
};
pub use policy_def::{
    DEFAULT_POLICY_NAME, FsCheckResult, FsOperation, FsProfile, PolicyDef, ResolvedFsProfile,
    UNRESTRICTED_FS_PROFILE,
};
pub use role::Role;
pub use timestamp::PolicyTimestamp;
