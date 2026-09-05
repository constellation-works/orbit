use std::fmt;
use std::str::FromStr;

use crate::identity::ReasoningEffort;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// v2 activity definition. Corresponds to the v2 YAML asset shape:
/// ```yaml
/// schemaVersion: 2
/// kind: Activity
/// metadata:
///   name: <name>
/// spec:
///   type: agent_loop | deterministic
///   description: <text>
///   input_schema_json: {...}
///   output_schema_json: {...}
///   tools: [...]
///   on_denial: terminate | continue  # agent_loop only; default terminate
///   ...type-specific fields
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityV2 {
    pub description: String,
    #[serde(default)]
    pub input_schema_json: Value,
    #[serde(default)]
    pub output_schema_json: Value,
    #[serde(rename = "fsProfile", default, skip_serializing_if = "Option::is_none")]
    pub fs_profile: Option<String>,
    #[serde(flatten)]
    pub spec: ActivityV2Spec,
}

/// v2 activity type discriminator. Serialized as
/// `type: agent_loop|deterministic`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityV2Spec {
    AgentLoop(AgentLoopSpec),
    Deterministic(DeterministicSpec),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentLoopSpec {
    /// System prompt / instruction delivered to the agent loop.
    #[serde(default)]
    pub instruction: String,
    /// Tool allowlist (§6). Empty means no tools are allowed.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Behavior when a denied tool is requested (§6 / §12 Q6).
    #[serde(default)]
    pub on_denial: OnDenial,
    /// Optional model override (provider-specific name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional provider-specific reasoning effort resolved from the selected
    /// crew. The agent configuration boundary rejects it for providers that
    /// do not support the Codex effort contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Upper bound on loop iterations. **Inert.**
    ///
    /// It bound only the engine-driven HTTP loop, which was retired with the
    /// `backend:` selector [ORB-10801]. The CLI agent runner never reads it:
    /// the provider owns its own turn budget and only
    /// `wall_clock_timeout_seconds` bounds the invocation.
    ///
    /// So do not declare it on an activity — it is inert config that reads as
    /// a live safety bound. [ORB-10382]
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Retired `backend:` selector [ORB-10801]. Agent execution runs through
    /// the CLI agent path only; there is no runtime backend choice left to
    /// make. The key is still parsed so an asset that declares the removed
    /// `http` / `auto` values fails closed at load instead of being silently
    /// reinterpreted as CLI execution.
    #[serde(default, skip_serializing)]
    pub backend: Option<RetiredAgentBackend>,
    /// Provider whose runtime executes this activity (§3.1).
    #[serde(default)]
    pub provider: Provider,
    /// Wall-clock timeout for a CLI agent invocation (§7.6).
    #[serde(default = "default_cli_wall_clock_timeout_seconds")]
    pub wall_clock_timeout_seconds: u64,
    /// Require a fully valid Orbit response envelope — exit alignment,
    /// `status: success`, object `result` — before a CLI invocation may report
    /// success. This is the **content contract**: it exists for activities that
    /// feed response fields into downstream templates. Defaults to `false` for
    /// artifact-backed activities, whose durable task/review/git state is
    /// authoritative.
    ///
    /// Distinct from [`AgentLoopSpec::require_completion_envelope`], which asks
    /// only whether the invocation finished at all. Setting this implies the
    /// completion check, since an absent envelope also fails validation.
    #[serde(default)]
    pub require_response_envelope: bool,
    /// Require the CLI invocation to *terminate with* a well-formed Orbit
    /// response envelope. This is the **step-completion protocol contract**
    /// ([ADR-0258] / [ORB-10449], amending ADR-0224), and it defaults to `true`.
    ///
    /// Every agent invocation is prompted with the response-envelope
    /// contract, so a provider that exits 0 with no envelope did not finish its
    /// turn — it stopped mid-work. Checkpointing that as success lets the run
    /// advance on work that never happened and defers the failure to whatever
    /// deterministic gate notices first, several steps downstream.
    ///
    /// The check is content-blind: it reads the envelope *frame* only
    /// (presence, `schemaVersion`, status token) and never `result` or `error`,
    /// so an agent that declares `status: "failed"` satisfies it. Agent-loop
    /// output remains advisory; only "did the contract complete" is enforced.
    ///
    /// Set to `false` only for an activity whose work is genuinely decorative —
    /// one whose non-completion should not stop the pipeline. Record why in the
    /// asset; the exception must be explicit, not incidental.
    #[serde(default = "default_require_completion_envelope")]
    pub require_completion_envelope: bool,
    /// Program allowlist enforced before `proc.spawn` executes a request.
    /// An empty `Some(vec![])` denies every program (fail-closed).
    ///
    /// `None` is only legal for an activity that does not grant `proc.spawn`:
    /// asset load rejects the pairing of a `proc.spawn` grant with a missing
    /// allowlist, so an author opts into deny-all by writing `[]` rather than
    /// getting allow-all by forgetting the key. The v2 activity tool context
    /// treats `None` as deny-all too. [ORB-10959]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proc_allowed_programs: Option<Vec<String>>,
}

/// Accepted-but-inert value of the retired `backend:` key [ORB-10801].
///
/// Orbit executes agent activities through the CLI agent path only, so the
/// selector no longer chooses anything. `cli` (the value every shipped asset
/// carried) still parses so existing assets keep loading unchanged; the
/// removed `http` and `auto` values are refused with the migration message
/// rather than being remapped onto CLI execution behind the operator's back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetiredAgentBackend;

/// Migration text shared by every surface that still accepts the retired
/// `backend` key, so an operator reads the same instruction from an asset, a
/// crew table, or `config.toml`.
pub const RETIRED_BACKEND_MIGRATION: &str = "agent execution runs through the CLI agent path only; remove the `backend` \
     setting (the value `cli` is still accepted and ignored)";

/// Validate one retired `backend` value. `Ok(())` for `cli`; the migration
/// error for anything else, including the removed `http` and `auto`.
pub fn check_retired_backend_value(raw: &str) -> Result<(), String> {
    match raw.trim() {
        "cli" => Ok(()),
        other => Err(format!(
            "`backend: {other}` is no longer supported: {RETIRED_BACKEND_MIGRATION}"
        )),
    }
}

impl<'de> Deserialize<'de> for RetiredAgentBackend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        check_retired_backend_value(&raw)
            .map(|()| RetiredAgentBackend)
            .map_err(serde::de::Error::custom)
    }
}

/// Named provider whose runtime executes an `agent_loop` activity. The enum is
/// closed-set: adding a provider means wiring a new CLI runtime factory, which
/// is a code change.
///
/// This is the **single canonical provider-identity surface** for Orbit
/// (ORB-10091): every crew/runtime path that turns a string into a provider —
/// crew parsing, activity resolution, CLI executor selection, setup
/// detection — routes through [`Provider::parse`] so canonical IDs, alias
/// normalization, and capability predicates cannot drift across layers or
/// disagree with Worker/Bridge. The set mirrors the Constellation
/// provider-resolution contract (see `docs/CONFIG.md` and the pinned fixture
/// under `tests/fixtures/provider_contract.json`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Claude,
    Codex,
    Gemini,
    Grok,
    Copilot,
    Ollama,
    #[serde(rename = "openai_compat", alias = "openai-compat")]
    OpenaiCompat,
    Cursor,
    Pi,
    Antigravity,
    Opencode,
}

/// One accepted non-canonical spelling for a [`Provider`]. Alias normalization
/// is table-driven so the accepted surface stays declarative and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderAlias {
    /// The accepted alternate spelling (already lower-cased / trimmed form).
    pub alias: &'static str,
    /// The canonical provider the alias resolves to.
    pub canonical: Provider,
    /// Whether the alias is deprecated. Callers may warn on a deprecated alias;
    /// resolution still succeeds so persisted identities are never broken.
    pub deprecated: bool,
}

/// Error returned when a provider identifier cannot be resolved to a canonical
/// [`Provider`]. Carries the offending raw string so callers can build stable,
/// non-silent diagnostics instead of falling back to a default runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderParseError {
    /// The raw input (as received, before normalization) that failed to parse.
    pub raw: String,
}

impl fmt::Display for ProviderParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown provider '{}'; expected one of {}",
            self.raw,
            Provider::CANONICAL_LIST
        )
    }
}

impl std::error::Error for ProviderParseError {}

impl Provider {
    /// Every canonical provider, in declaration order. Adding a variant here is
    /// a compile-time forcing function for the match arms below.
    pub const ALL: [Provider; 11] = [
        Provider::Claude,
        Provider::Codex,
        Provider::Gemini,
        Provider::Grok,
        Provider::Copilot,
        Provider::Ollama,
        Provider::OpenaiCompat,
        Provider::Cursor,
        Provider::Pi,
        Provider::Antigravity,
        Provider::Opencode,
    ];

    /// Accepted non-canonical spellings, normalized by [`Provider::parse`] /
    /// [`Provider::resolve_name`]. The five legacy **vendor** names are the
    /// deprecated aliases from the Constellation provider-resolution contract
    /// (§2): they resolve successfully but carry an observable deprecation
    /// signal. `openai-compat` is a non-deprecated spelling variant of the
    /// canonical `openai_compat` id (it is also a serde alias on the enum).
    /// This table is **closed** — an unlisted string is `provider.unknown`,
    /// never guessed. New aliases require a contract bump.
    ///
    /// `copilot`, `cursor`, `pi`, `antigravity`, and `opencode` deliberately
    /// have **no** alias rows.
    /// Neither the platform name nor a selected underlying model vendor may
    /// resolve to a different execution lane. This matters most for `pi`,
    /// whose own `--provider` flag names the model vendor (`anthropic`,
    /// `openai`, ...) *inside* the Pi lane: those vendor spellings already
    /// resolve to other Orbit executors and must keep doing so. The same holds
    /// for `opencode`, whose `--model provider/model` argument names the model
    /// vendor *inside* the OpenCode lane. [ORB-10946] [ORB-10945] [ORB-11296]
    /// [ORB-11299] [ORB-11295]
    pub const ALIASES: &'static [ProviderAlias] = &[
        ProviderAlias {
            alias: "anthropic",
            canonical: Provider::Claude,
            deprecated: true,
        },
        ProviderAlias {
            alias: "openai",
            canonical: Provider::Codex,
            deprecated: true,
        },
        ProviderAlias {
            alias: "chatgpt",
            canonical: Provider::Codex,
            deprecated: true,
        },
        ProviderAlias {
            alias: "google",
            canonical: Provider::Gemini,
            deprecated: true,
        },
        ProviderAlias {
            alias: "xai",
            canonical: Provider::Grok,
            deprecated: true,
        },
        ProviderAlias {
            alias: "openai-compat",
            canonical: Provider::OpenaiCompat,
            deprecated: false,
        },
    ];

    /// Human-readable canonical id list used in diagnostics.
    pub const CANONICAL_LIST: &'static str = "claude, codex, gemini, grok, copilot, ollama, openai_compat, cursor, pi, antigravity, opencode";

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Gemini => "gemini",
            Provider::Grok => "grok",
            Provider::Copilot => "copilot",
            Provider::Ollama => "ollama",
            Provider::OpenaiCompat => "openai_compat",
            Provider::Cursor => "cursor",
            Provider::Pi => "pi",
            Provider::Antigravity => "antigravity",
            Provider::Opencode => "opencode",
        }
    }

    /// Canonical parse with alias normalization, **preserving** the observable
    /// alias/deprecation metadata (contract §2). Trims surrounding whitespace
    /// and lower-cases before matching, so config/env casing variants resolve
    /// to the same identity (`"  OpenAI "` → `codex` + deprecation signal).
    /// Unknown strings return [`ProviderParseError`] — this is the *only*
    /// string→provider entry point; callers must not invent their own match and
    /// must not silently fall back on error.
    ///
    /// Prefer this over [`Provider::parse`] wherever the caller can surface the
    /// deprecation (e.g. a config warn-log): `parse` discards the signal.
    pub fn resolve_name(raw: &str) -> Result<ProviderIdentity, ProviderParseError> {
        let normalized = raw.trim().to_ascii_lowercase();
        if let Some(provider) = Provider::ALL
            .into_iter()
            .find(|provider| provider.as_str() == normalized)
        {
            return Ok(ProviderIdentity {
                provider,
                deprecation: None,
            });
        }
        if let Some(alias) = Provider::ALIASES
            .iter()
            .find(|alias| alias.alias == normalized)
        {
            let deprecation = alias.deprecated.then(|| ProviderDeprecation {
                // The signal carries the normalized alias spelling, not the raw
                // casing, so `"OpenAI"` and `"openai"` produce the same signal.
                alias: normalized.clone(),
                canonical: alias.canonical,
            });
            return Ok(ProviderIdentity {
                provider: alias.canonical,
                deprecation,
            });
        }
        Err(ProviderParseError {
            raw: raw.to_string(),
        })
    }

    /// Canonical parse that discards alias/deprecation metadata. Thin wrapper
    /// over [`Provider::resolve_name`] for the many call sites that only need
    /// the canonical identity. Same normalization and same no-fallback error.
    pub fn parse(raw: &str) -> Result<Provider, ProviderParseError> {
        Provider::resolve_name(raw).map(|identity| identity.provider)
    }

    /// Whether Orbit ships a CLI runtime for this provider. `openai_compat`
    /// has none, so selecting it must fail structurally rather than fall back.
    /// All other canonical providers have a CLI runtime.
    pub fn has_cli_runtime(self) -> bool {
        !matches!(self, Provider::OpenaiCompat)
    }

    /// Whether the model-neutral Worker leaf executor can execute this
    /// provider. Worker only wires the four shared CLI agent families;
    /// `copilot`, `cursor`, `pi`, `antigravity`, `opencode`, `ollama`, and
    /// `openai_compat` are Orbit-canonical capabilities Worker does not run.
    /// Preserving this distinction is an explicit ORB-10091 constraint — Orbit
    /// keeps the wider set even though Worker cannot execute all of it.
    ///
    /// [ORB-10946] `copilot` returning `false` here is the stable
    /// unavailable diagnostic, not a fallback: a Worker-routed step naming
    /// `copilot` is refused by identity rather than silently re-pointed at
    /// another family.
    pub fn is_worker_executable(self) -> bool {
        matches!(
            self,
            Provider::Claude | Provider::Codex | Provider::Gemini | Provider::Grok
        )
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Provider {
    type Err = ProviderParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Provider::parse(value)
    }
}

/// A resolved provider identity plus any observable deprecation signal emitted
/// while normalizing a legacy alias (contract §2). Returned by
/// [`Provider::resolve_name`] so callers can warn on a deprecated alias without
/// losing the fact that normalization occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentity {
    /// The canonical provider the input normalized to.
    pub provider: Provider,
    /// `Some` when the input was a **deprecated** alias; `None` for a canonical
    /// id or a non-deprecated spelling variant.
    pub deprecation: Option<ProviderDeprecation>,
}

/// Observable deprecation signal: a legacy alias was normalized to a canonical
/// provider. Resolution still succeeds (contract §2); callers surface this as a
/// warning carrying `{alias, canonical}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderDeprecation {
    /// The normalized (trimmed, lower-cased) alias spelling that was accepted.
    pub alias: String,
    /// The canonical provider it resolved to.
    pub canonical: Provider,
}

/// Which entry point's capability set applies during full resolution
/// (contract §5). The canonical four providers are executable at every entry
/// point; `ollama` / `openai_compat` are known Orbit identities that no CLI
/// entry point can execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderEntryPoint {
    Orbit,
    Worker,
    Bridge,
}

/// The precedence tier that supplied the resolved value (contract §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    Explicit,
    TaskConfig,
    WorkspaceDefault,
    EnvironmentDefault,
    SystemDefault,
    PersistedReconciliation,
}

impl ProviderSource {
    /// Stable contract spelling used in diagnostics / conformance assertions.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderSource::Explicit => "explicit",
            ProviderSource::TaskConfig => "task_config",
            ProviderSource::WorkspaceDefault => "workspace_default",
            ProviderSource::EnvironmentDefault => "environment_default",
            ProviderSource::SystemDefault => "system_default",
            ProviderSource::PersistedReconciliation => "persisted_reconciliation",
        }
    }
}

/// Stable diagnostic code for a resolution outcome (contract §6). Each repo
/// maps its native error/log to the shared code; conformance asserts the code,
/// not the wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderDiagnostic {
    Ok,
    Defaulted,
    AliasDeprecated,
    Unknown,
    Unsupported,
    Unavailable,
}

impl ProviderDiagnostic {
    /// Stable contract code string (e.g. `"provider.alias_deprecated"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderDiagnostic::Ok => "provider.ok",
            ProviderDiagnostic::Defaulted => "provider.defaulted",
            ProviderDiagnostic::AliasDeprecated => "provider.alias_deprecated",
            ProviderDiagnostic::Unknown => "provider.unknown",
            ProviderDiagnostic::Unsupported => "provider.unsupported",
            ProviderDiagnostic::Unavailable => "provider.unavailable",
        }
    }

    /// Whether the diagnostic represents a successful resolution (§6).
    pub fn is_success(self) -> bool {
        matches!(
            self,
            ProviderDiagnostic::Ok
                | ProviderDiagnostic::Defaulted
                | ProviderDiagnostic::AliasDeprecated
        )
    }
}

/// Inputs to a single provider resolution (contract §8). Every tier is an
/// optional raw string (empty / whitespace-only counts as "not selected" and
/// falls through, not an error). `host_available` is `None` when availability
/// is not exercised.
#[derive(Debug, Clone)]
pub struct ProviderResolveRequest<'a> {
    pub entry_point: ProviderEntryPoint,
    pub requested: Option<&'a str>,
    pub task_provider: Option<&'a str>,
    pub workspace_default: Option<&'a str>,
    pub env_default: Option<&'a str>,
    pub system_default: Option<&'a str>,
    pub persisted_resolution: Option<&'a str>,
    pub host_available: Option<bool>,
}

impl ProviderResolveRequest<'_> {
    /// A request with only the entry point set; fill tiers with struct-update
    /// syntax (`ProviderResolveRequest { requested: Some(x), ..base }`).
    pub fn new(entry_point: ProviderEntryPoint) -> Self {
        Self {
            entry_point,
            requested: None,
            task_provider: None,
            workspace_default: None,
            env_default: None,
            system_default: None,
            persisted_resolution: None,
            host_available: None,
        }
    }
}

/// Outcome of a provider resolution (contract §8): the normalized identity (or
/// `None` on `provider.unknown`), the tier it came from, the stable diagnostic,
/// and any deprecation signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderResolution {
    pub normalized_provider: Option<Provider>,
    pub source: ProviderSource,
    pub diagnostic: ProviderDiagnostic,
    pub deprecation: Option<ProviderDeprecation>,
}

impl ProviderResolution {
    /// Whether resolution succeeded (identity usable for dispatch).
    pub fn is_success(&self) -> bool {
        self.diagnostic.is_success()
    }
}

impl Provider {
    /// Providers this entry point can execute (contract §5 capability set). The
    /// canonical four are executable everywhere; `ollama` / `openai_compat` are
    /// known but unsupported at every CLI entry point — the identity resolves,
    /// only execution capability is missing.
    pub fn capabilities(_entry_point: ProviderEntryPoint) -> &'static [Provider] {
        const CANONICAL: [Provider; 4] = [
            Provider::Claude,
            Provider::Codex,
            Provider::Gemini,
            Provider::Grok,
        ];
        &CANONICAL
    }

    /// Full contract resolution (§8): reconciliation short-circuit, then
    /// precedence (§3), alias normalization (§2), and identity → capability →
    /// availability checks (§5) — never falling back to another provider. This
    /// is the surface the vendored conformance cases assert against, so Orbit
    /// stays in parity with Worker and Bridge.
    pub fn resolve(request: &ProviderResolveRequest<'_>) -> ProviderResolution {
        // Reconciliation short-circuit: a frozen resolution is reused verbatim
        // and precedence is not re-run (§7).
        if let Some(persisted) = non_empty(request.persisted_resolution) {
            // A persisted identity is already canonical; if it somehow fails to
            // parse we surface `unknown` rather than inventing a fallback.
            return match Provider::parse(persisted) {
                Ok(provider) => ProviderResolution {
                    normalized_provider: Some(provider),
                    source: ProviderSource::PersistedReconciliation,
                    diagnostic: ProviderDiagnostic::Ok,
                    deprecation: None,
                },
                Err(_) => ProviderResolution {
                    normalized_provider: None,
                    source: ProviderSource::PersistedReconciliation,
                    diagnostic: ProviderDiagnostic::Unknown,
                    deprecation: None,
                },
            };
        }

        // Precedence: first non-empty tier wins (§3).
        let tiers = [
            (request.requested, ProviderSource::Explicit),
            (request.task_provider, ProviderSource::TaskConfig),
            (request.workspace_default, ProviderSource::WorkspaceDefault),
            (request.env_default, ProviderSource::EnvironmentDefault),
            (request.system_default, ProviderSource::SystemDefault),
        ];
        let Some((raw, source)) = tiers
            .into_iter()
            .find_map(|(value, source)| non_empty(value).map(|raw| (raw, source)))
        else {
            // No tier supplied a value. The contract always provides a system
            // default (canonical `claude`); treat an absent one as that so
            // resolution is total.
            return ProviderResolution {
                normalized_provider: Some(Provider::default()),
                source: ProviderSource::SystemDefault,
                diagnostic: ProviderDiagnostic::Defaulted,
                deprecation: None,
            };
        };

        // Normalize (§2), then identity → capability → availability (§5).
        let identity = match Provider::resolve_name(raw) {
            Ok(identity) => identity,
            Err(_) => {
                return ProviderResolution {
                    normalized_provider: None,
                    source,
                    diagnostic: ProviderDiagnostic::Unknown,
                    deprecation: None,
                };
            }
        };

        if !Provider::capabilities(request.entry_point).contains(&identity.provider) {
            return ProviderResolution {
                normalized_provider: Some(identity.provider),
                source,
                diagnostic: ProviderDiagnostic::Unsupported,
                deprecation: None,
            };
        }

        if request.host_available == Some(false) {
            return ProviderResolution {
                normalized_provider: Some(identity.provider),
                source,
                diagnostic: ProviderDiagnostic::Unavailable,
                deprecation: None,
            };
        }

        let diagnostic = if identity.deprecation.is_some() {
            ProviderDiagnostic::AliasDeprecated
        } else if source == ProviderSource::Explicit {
            ProviderDiagnostic::Ok
        } else {
            ProviderDiagnostic::Defaulted
        };
        ProviderResolution {
            normalized_provider: Some(identity.provider),
            source,
            diagnostic,
            deprecation: identity.deprecation,
        }
    }
}

/// Trim a tier value and treat empty / whitespace-only as "not selected" (§3).
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeterministicSpec {
    /// Registered deterministic action name. The dispatcher resolves this
    /// through the engine's built-in action table at runtime.
    pub action: String,
    /// Optional literal configuration passed through to the action.
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnDenial {
    /// Terminate the loop on a denied tool call (default per §12 Q6).
    #[default]
    Terminate,
    /// Continue the loop with the structured tool-result error.
    Continue,
}

const fn default_max_iterations() -> u32 {
    25
}

const fn default_cli_wall_clock_timeout_seconds() -> u64 {
    300
}

/// [ORB-10449] Fail closed. An agent-loop activity that omits the field gets
/// the step-completion protocol check, so a new or legacy asset cannot silently
/// inherit the pre-ORB-10449 behaviour of checkpointing a stalled agent as
/// success.
const fn default_require_completion_envelope() -> bool {
    true
}
