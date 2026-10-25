//! Single source of truth for Orbit's default model names.
//!
//! Default model names used to be hardcoded as version-pinned string literals
//! scattered across crates, and had drifted out of sync — the Claude default
//! appeared as `claude-opus-4-7` in one place, `claude-sonnet-4-6` in another,
//! and `claude-sonnet-4-5` in a third. This module centralizes the production
//! defaults so a single edit updates every code path and the drift cannot
//! recur.
//!
//! ## Asset ↔ const seam
//!
//! YAML/TOML assets (including seeded `config.toml`) cannot reference a Rust
//! const. For those, "single source of truth" means using the provider alias
//! directly in the asset and keeping this module authoritative for production
//! Rust paths. Shipped executor assets select models through crews and
//! `model_flag`; the legacy executor model-pair override is retained only for
//! compatibility with older/user-authored definitions.
//!
//! ## Aliases vs. version pins
//!
//! The Claude CLI accepts the unversioned `opus`/`sonnet`/`haiku` aliases and resolves
//! them to the current flagship, so the CLI defaults never drift. codex,
//! gemini, and grok use provider-specific version pins because no stable
//! unversioned aliases are available for those CLIs.

/// Default "strong" Claude model: the unversioned `opus` CLI alias.
pub const CLAUDE_DEFAULT_STRONG: &str = "opus";

/// Default "weak" Claude model: the unversioned `sonnet` CLI alias.
pub const CLAUDE_DEFAULT_WEAK: &str = "sonnet";

/// Claude CLI alias used by the standard Haiku crew: the cheap tier, which
/// resolves to the current Haiku release (Claude CLI 2.1.293 reports
/// `claude-haiku-5-5`).
pub const CLAUDE_HAIKU_MODEL: &str = "haiku";

/// Claude CLI alias used by the standard Fable crew.
pub const CLAUDE_FABLE_MODEL: &str = "fable";

/// Codex model used by the standard Sol crew.
pub const CODEX_SOL_MODEL: &str = "gpt-6.1-sol";

/// Codex model used by the standard Luna crew.
pub const CODEX_LUNA_MODEL: &str = "gpt-6-luna";

/// Codex model used by the standard Astra crew and provider default.
pub const CODEX_ASTRA_MODEL: &str = "gpt-6-astra";

/// Default Antigravity (`agy`) model: a slug from `agy models` (verified
/// against Antigravity CLI 1.1.27). Bare Gemini CLI ids such as
/// `gemini-3.8-flash` are not remapped. [ORB-11299]
pub const ANTIGRAVITY_DEFAULT_MODEL: &str = "gemini-3.8-flash-high";

/// Antigravity (`agy`) model for the built-in `argon` crew: a slug from
/// `agy models`, verified against Antigravity CLI 1.4.7 (`agy --version`),
/// which lists `gemini-4-argon-high` alongside the Gemini 3.8 Flash slugs.
pub const ANTIGRAVITY_ARGON_MODEL: &str = "gemini-4-argon-high";

/// Default Grok Build model (the canonical model listed by `grok models`).
pub const GROK_DEFAULT_MODEL: &str = "grok-4.7";

/// Default model for the GitHub Copilot CLI lane.
///
/// Copilot routes to several vendors' models; Orbit pins an explicit id rather
/// than letting the CLI fall back to `COPILOT_MODEL` or its persisted `/model`
/// choice, so a run's model comes from the resolved crew and not from ambient
/// operator state. Both ids below were verified against the account-visible
/// model catalog in Copilot CLI 1.0.84. The provider identity stays `copilot`
/// regardless of which vendor supplies the model. [ORB-10946]
pub const COPILOT_DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Default model for the Muse Code execution lane.
///
/// Muse Code is Meta's terminal coding agent; its `--model` takes an id from
/// the Muse model catalog (`muse model-profile show <model>` resolves one).
/// The executor identity stays `muse` whichever model the crew selects, so
/// the lane is never inferred from this string. [ORB-16577]
pub const MUSE_DEFAULT_MODEL: &str = "muse-spark-1.3";

/// Muse Code's mid-tier model, the built-in `muse-wonder` crew's.
pub const MUSE_WONDER_MODEL: &str = "muse-wonder-1.0";

/// Muse Code's strongest model, the built-in `muse-wonder-full` crew's.
pub const MUSE_WONDER_FULL_MODEL: &str = "muse-wonder-full-1.0";
