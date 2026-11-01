use std::path::{Path, PathBuf};

/// The two directories a layered configuration is read from.
///
/// This crate never discovers roots for itself: there is no cwd walk, no
/// `$HOME` probe, and no dependency on Orbit's path resolution. Composition
/// layers resolve both roots and hand them in, which is what makes config
/// loading deterministic under test and reusable outside the runtime.
///
/// `global` is the machine-wide root (`~/.orbit`), `workspace` the
/// workspace-local one (`<repo>/.orbit`). The two are equal when a caller
/// deliberately reads a single root as both layers; layering then skips the
/// workspace file entirely rather than reading the same document twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRoots {
    global: PathBuf,
    workspace: PathBuf,
}

impl ConfigRoots {
    /// Layer `workspace` over `global`.
    pub fn new(global: impl Into<PathBuf>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            global: global.into(),
            workspace: workspace.into(),
        }
    }

    /// Read one root as both layers, for callers that hold no workspace root
    /// (global-only inspection such as `orbit migrate --dry-run`).
    pub fn global_only(global: impl Into<PathBuf>) -> Self {
        let global = global.into();
        Self {
            workspace: global.clone(),
            global,
        }
    }

    /// The machine-wide root.
    pub fn global(&self) -> &Path {
        &self.global
    }

    /// The workspace-local root.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Whether a distinct workspace layer exists. Replace-only keys resolve to
    /// built-in defaults rather than global values only when this is true.
    pub fn has_workspace_layer(&self) -> bool {
        if self.workspace == self.global {
            return false;
        }
        match (self.workspace.canonicalize(), self.global.canonicalize()) {
            (Ok(workspace), Ok(global)) => workspace != global,
            _ => true,
        }
    }
}
