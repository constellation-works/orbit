//! Scope resolution: how the workspace and global layers compose for one key.
//!
//! A store declares its [`ScopeStrategy`] through [`ScopedStore`], and
//! [`resolve`] applies that strategy to both layers. Each asset type maps to
//! exactly one strategy, with no per-key overrides and no runtime policy. The
//! mapping is the table under `## Scoping rules` in `ARCHITECTURE.md`; keep the
//! two in step.

/// Strategy describing how workspace and global layers compose for a given
/// asset type. See the table under `## Scoping rules` in `ARCHITECTURE.md`
/// for the canonical mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeStrategy {
    /// Workspace layer only; global is ignored. Used for tasks and job runs.
    WorkspaceOnly,
    /// Global provides defaults; workspace entries with matching keys
    /// override. Used for activities, jobs, and skills.
    MergeByKey,
    /// Single authoritative layer (global); workspace is ignored. Used for
    /// the audit trail.
    GlobalOnly,
}

/// Contract surfaced by a scoped store so [`resolve`] can compose its two
/// layers without knowing anything about the concrete backend. Lookups are
/// fallible (the underlying backends can fail for I/O reasons), so getters
/// return `Result<Option<T>, E>` with an error type chosen by the store.
pub trait ScopedStore<T> {
    /// Error type propagated from the underlying backend.
    type Err;
    /// Strategy governing this store's composition.
    fn strategy(&self) -> ScopeStrategy;
    /// Look up the workspace layer. `Ok(None)` when not present.
    fn get_workspace(&self, key: &str) -> Result<Option<T>, Self::Err>;
    /// Look up the global layer. `Ok(None)` when not present.
    fn get_global(&self, key: &str) -> Result<Option<T>, Self::Err>;
}

/// Resolve a key to its composed value by applying the store's strategy.
/// Returns `Ok(None)` when no layer the strategy reads holds the key.
pub fn resolve<T, S: ScopedStore<T>>(store: &S, key: &str) -> Result<Option<T>, S::Err> {
    match store.strategy() {
        ScopeStrategy::WorkspaceOnly => store.get_workspace(key),
        ScopeStrategy::GlobalOnly => store.get_global(key),
        ScopeStrategy::MergeByKey => {
            // Workspace entries with matching keys override global defaults.
            // A full merged listing collects keys from both layers, then
            // resolves each one; resolve() is for single-key lookups.
            if let Some(value) = store.get_workspace(key)? {
                return Ok(Some(value));
            }
            store.get_global(key)
        }
    }
}
