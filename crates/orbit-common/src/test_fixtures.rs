//! Frozen model-name constants for tests.
//!
//! These keep test fixtures, attribution values, and snapshots stable and
//! independent of the production defaults in [`crate::model_defaults`]. The
//! values are intentionally *frozen* at the literals that were hardcoded
//! before centralization, so mechanical fixture/snapshot data does not churn
//! when production defaults change.
//!
//! Guidance:
//! - A test that asserts on a **default fallback or seeded output** should use
//!   the production constant from [`crate::model_defaults`] (its expected value
//!   tracks the real default).
//! - A test where the model is **mere input or attribution** (who authored a
//!   task/comment, an arbitrary `--model` pass-through) should use one of these
//!   frozen constants so the fixture value never changes.
//!
//! Always available so integration tests in sibling crates can share the same
//! constants without changing `orbit-common`'s feature set.

/// Frozen Claude model literal for attribution/input test fixtures.
pub const TEST_CLAUDE_MODEL: &str = "claude-opus-4-7";

/// Frozen weak-Claude model literal for attribution/input test fixtures.
pub const TEST_CLAUDE_WEAK_MODEL: &str = "claude-sonnet-4-6";

/// Frozen codex model literal for attribution/input test fixtures.
pub const TEST_CODEX_MODEL: &str = "gpt-5.5";

/// Frozen gemini model literal for attribution/input test fixtures.
pub const TEST_GEMINI_MODEL: &str = "gemini-3.1-pro";

/// Frozen grok model literal for attribution/input test fixtures.
pub const TEST_GROK_MODEL: &str = "grok-4";

/// Convenience default test model (codex family) for tests that just need
/// *some* model string and do not care which.
pub fn test_model() -> &'static str {
    TEST_CODEX_MODEL
}
