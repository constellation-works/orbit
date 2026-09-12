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
//! The Claude CLI accepts the unversioned `opus`/`sonnet` aliases and resolves
//! them to the current flagship, so the CLI defaults never drift. The Anthropic
//! **HTTP Messages API** rejects bare aliases and requires a fully-qualified
//! model id, so [`ANTHROPIC_HTTP_DEFAULT_MODEL`] stays version pinned. codex,
//! gemini, and grok use provider-specific version pins because no stable
//! unversioned aliases are available for those CLIs.

/// Default "strong" Claude model: the unversioned `opus` CLI alias.
pub const CLAUDE_DEFAULT_STRONG: &str = "opus";

/// Default "weak" Claude model: the unversioned `sonnet` CLI alias.
pub const CLAUDE_DEFAULT_WEAK: &str = "sonnet";

/// Claude CLI alias used by the standard Fable crew.
pub const CLAUDE_FABLE_MODEL: &str = "fable";

/// Default model for the Anthropic **HTTP Messages API** path.
///
/// Unlike the CLI, the Messages API requires a fully-qualified model id and
/// rejects bare aliases like `opus`/`sonnet`, so this default stays version
/// pinned. Bump it in lockstep with Anthropic's published API model ids.
pub const ANTHROPIC_HTTP_DEFAULT_MODEL: &str = "claude-sonnet-4-5";

/// Codex model used by the standard Sol crew.
pub const CODEX_SOL_MODEL: &str = "gpt-5.6-sol";

/// Codex model used by the standard Terra crew and provider default.
pub const CODEX_TERRA_MODEL: &str = "gpt-5.6-terra";

/// Codex model used by the standard Luna crew.
pub const CODEX_LUNA_MODEL: &str = "gpt-5.6-luna";

/// Codex model used by the standard Astra crew and provider default.
pub const CODEX_ASTRA_MODEL: &str = "gpt-6-astra";

/// Default codex model (Astra is the provider default).
pub const CODEX_DEFAULT_MODEL: &str = CODEX_ASTRA_MODEL;

/// Legacy codex "weak" model retained for compatibility with executor pairs.
pub const CODEX_DEFAULT_WEAK: &str = "gpt-5.4-mini";

/// Default gemini model for the provider-default map.
pub const GEMINI_DEFAULT_MODEL: &str = "gemini-3.8-flash";

/// Default gemini model seeded into crew roles.
pub const GEMINI_CREW_MODEL: &str = "gemini-3.8-flash";

/// Default Antigravity (`agy`) model: a slug from `agy models` (verified
/// against Antigravity CLI 1.1.27). Bare Gemini CLI ids such as
/// `gemini-3.8-flash` are not remapped. [ORB-11299]
pub const ANTIGRAVITY_DEFAULT_MODEL: &str = "gemini-3.8-flash-high";

/// Cheap-tier Antigravity model used for the bounded system crew. Flash-low
/// is the documented low-effort sibling of the default high slug.
pub const ANTIGRAVITY_CREW_MODEL: &str = "gemini-3.8-flash-low";

/// Legacy gemini "strong" model retained for compatibility with executor pairs.
pub const GEMINI_PAIR_STRONG: &str = "gemini-3.1-pro";

/// Legacy gemini "weak" model retained for compatibility with executor pairs.
pub const GEMINI_PAIR_WEAK: &str = "gemini-3.8-flash";

/// Default Grok Build model (the canonical model listed by `grok models`).
pub const GROK_DEFAULT_MODEL: &str = "grok-4.6";

/// Default model for the GitHub Copilot CLI lane.
///
/// Copilot routes to several vendors' models; Orbit pins an explicit id rather
/// than letting the CLI fall back to `COPILOT_MODEL` or its persisted `/model`
/// choice, so a run's model comes from the resolved crew and not from ambient
/// operator state. Both ids below were verified against the account-visible
/// model catalog in Copilot CLI 1.0.84. The provider identity stays `copilot`
/// regardless of which vendor supplies the model. [ORB-10946]
pub const COPILOT_DEFAULT_MODEL: &str = "claude-sonnet-5";

/// Cheap-tier Claude model used for the bounded Copilot system crew.
pub const COPILOT_CREW_MODEL: &str = "claude-haiku-4.5";

/// Default model for the Cursor execution lane.
///
/// Cursor routes to multiple model vendors, but the persisted provider remains
/// `cursor`. Orbit passes this documented current CLI model id explicitly so a
/// run never depends on an interactive session's ambient model selection.
/// [ORB-10945]
pub const CURSOR_DEFAULT_MODEL: &str = "gpt-5";

/// Model used for Cursor's bounded system crew. Cursor does not publish a
/// stable cheap-tier alias, so the known-good default is reused.
pub const CURSOR_CREW_MODEL: &str = CURSOR_DEFAULT_MODEL;

/// Default model for the Pi execution lane.
///
/// Pi routes to many model vendors and resolves `--model` as a *pattern*
/// against a catalog it refreshes on its own schedule, so a version-pinned id
/// would rot faster than the alias does. `sonnet` is the unversioned pattern
/// Pi's own documented examples use. The persisted provider identity stays
/// `pi` whichever vendor the pattern resolves to. [ORB-11296]
pub const PI_DEFAULT_MODEL: &str = "sonnet";

/// Model used for Pi's bounded system crew. Pi publishes no stable cheap-tier
/// alias of its own, so the known-good default is reused; an operator who
/// wants a cheaper tier names one explicitly in `[crews.system]`.
pub const PI_CREW_MODEL: &str = PI_DEFAULT_MODEL;

/// Default model for the OpenCode execution lane.
///
/// OpenCode addresses models as a `provider/model` coordinate, where the
/// leading segment names the *model vendor* inside the OpenCode lane — never
/// the Orbit executor, which stays `opencode` whichever vendor is selected.
/// The coordinate must be fully qualified: OpenCode splits on the first `/`
/// and looks the vendor up in its provider catalog, so a bare model id does
/// not resolve. [ORB-11295]
pub const OPENCODE_DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4-5";

/// Model used for OpenCode's bounded system crew. OpenCode publishes no
/// vendor-independent cheap tier, so this names the cheap tier of the same
/// vendor as [`OPENCODE_DEFAULT_MODEL`]; an operator who prefers another
/// vendor names one explicitly in `[crews.system]`.
pub const OPENCODE_CREW_MODEL: &str = "anthropic/claude-haiku-4-5";

/// Cheap Claude model used by the orbit-agent HTTP examples.
///
/// Version pinned like [`ANTHROPIC_HTTP_DEFAULT_MODEL`] because the examples
/// hit the Anthropic Messages API directly, which rejects bare aliases.
pub const ANTHROPIC_EXAMPLE_MODEL: &str = "claude-haiku-4-5-20251001";

/// Provider → default model used to seed prompt defaults.
///
/// Mirrors the historical `agent_detect::default_model_for` map; `claude` now
/// resolves to the unversioned [`CLAUDE_DEFAULT_STRONG`] alias. codex/gemini/
/// grok/copilot/cursor/pi use their provider-specific defaults.
pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "claude" => Some(CLAUDE_DEFAULT_STRONG),
        "codex" => Some(CODEX_DEFAULT_MODEL),
        "gemini" => Some(GEMINI_DEFAULT_MODEL),
        "antigravity" => Some(ANTIGRAVITY_DEFAULT_MODEL),
        "grok" => Some(GROK_DEFAULT_MODEL),
        "copilot" => Some(COPILOT_DEFAULT_MODEL),
        "cursor" => Some(CURSOR_DEFAULT_MODEL),
        "pi" => Some(PI_DEFAULT_MODEL),
        _ => None,
    }
}
