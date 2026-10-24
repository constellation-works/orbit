use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::physical::resolve_symlinks;
use orbit_types::policy::{FsOperation, PolicyDef};

use crate::evaluator;

/// `matched_rule` recorded when a resolved path escapes the workspace root.
const OUTSIDE_WORKSPACE: &str = "<outside workspace>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsPolicyEvaluation {
    pub profile: String,
    pub operation: FsOperation,
    pub path: String,
    pub allowed: bool,
    pub matched_rule: String,
}

#[derive(Debug, Clone)]
pub struct PolicyEngine {
    def: PolicyDef,
}

impl PolicyEngine {
    pub fn from_def(def: &PolicyDef) -> Result<Self, OrbitError> {
        def.validate()?;
        Ok(Self { def: def.clone() })
    }

    pub fn check(
        &self,
        profile: impl Into<String>,
        operation: FsOperation,
        path: impl Into<String>,
    ) -> Result<FsPolicyEvaluation, OrbitError> {
        let profile = profile.into();
        let path = path.into();
        let result = evaluator::evaluate(&self.def, &profile, operation, &path)?;
        Ok(FsPolicyEvaluation {
            profile,
            operation,
            path,
            allowed: result.allowed,
            matched_rule: result.matched_rule,
        })
    }

    /// Symlink-safe policy evaluation. [ORB-00418]
    ///
    /// Resolves `requested_path` — following symlinks — against
    /// `workspace_root` before matching rules, so a symlink inside an allowed
    /// subtree that points into a denied subtree is DENIED: rules match the
    /// *real* filesystem location, not the link path. Existing paths are
    /// canonicalized directly; for a not-yet-existing target (a write/create)
    /// the nearest existing ancestor is canonicalized (resolving symlinks in
    /// the ancestor) and the remaining components are rejoined.
    ///
    /// A resolved path that escapes the workspace root is denied outright.
    ///
    /// Unlike [`Self::check`] (which matches the caller-supplied path string as
    /// given), this method owns symlink resolution so the guarantee does not
    /// depend on every caller canonicalizing first. On non-sandboxed platforms
    /// (Linux) the policy check is the only enforcement layer, so resolving
    /// here is load-bearing. TOCTOU between this check and the actual
    /// filesystem access is an OS-level concern and out of scope.
    pub fn check_resolved(
        &self,
        workspace_root: &Path,
        profile: impl Into<String>,
        operation: FsOperation,
        requested_path: &Path,
    ) -> Result<FsPolicyEvaluation, OrbitError> {
        let profile = profile.into();
        let resolved = resolve_symlinks(requested_path)?;
        let canonical_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());

        let Ok(relative) = resolved.strip_prefix(&canonical_root) else {
            // The resolved path (e.g. a symlink target) landed outside the
            // workspace root entirely — deny, don't fall through to rules that
            // only govern in-workspace paths.
            return Ok(FsPolicyEvaluation {
                profile,
                operation,
                path: resolved.to_string_lossy().replace('\\', "/"),
                allowed: false,
                matched_rule: OUTSIDE_WORKSPACE.to_string(),
            });
        };

        let relative = workspace_relative_string(relative);
        let result = evaluator::evaluate(&self.def, &profile, operation, &relative)?;
        Ok(FsPolicyEvaluation {
            profile,
            operation,
            path: relative,
            allowed: result.allowed,
            matched_rule: result.matched_rule,
        })
    }

    pub fn def(&self) -> &PolicyDef {
        &self.def
    }
}

/// Render a workspace-relative path as the `./…` form the rule matcher expects
/// (mirrors the projection used by callers that pre-relativize).
fn workspace_relative_string(relative: &Path) -> String {
    let rendered = relative.to_string_lossy().replace('\\', "/");
    if rendered.is_empty() {
        ".".to_string()
    } else {
        format!("./{rendered}")
    }
}
