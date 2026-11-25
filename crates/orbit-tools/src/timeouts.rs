//! Shared network and subprocess timeout constants for builtin, external, and plugin tools; imports no internal modules.

/// Default network operation timeout (15 s). Used for most GitHub API calls
/// and Orbit CLI commands where a quick response is expected.
pub(crate) const TIMEOUT_DEFAULT_MS: u64 = 15_000;

/// Slow operation timeout (30 s). Used for git network operations and PR creation,
/// which may involve larger payloads or slower remotes.
pub(crate) const TIMEOUT_SLOW_MS: u64 = 30_000;

/// Long operation timeout (60 s). Used for `gh pr checkout`, which clones or
/// fetches a branch and may transfer significant data over the network.
pub(crate) const TIMEOUT_LONG_MS: u64 = 60_000;
