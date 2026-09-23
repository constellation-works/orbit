//! Canonical actor identity for audit events (ORB-10888).
//!
//! The `audit_events.role` column is a single free-text label that conflates
//! five unrelated kinds of value: agent families (`claude`), model strings
//! (`claude-opus-5`, `opus`), system/synthetic markers (`admin`, `hook`),
//! unattributed markers (`unknown`, `unverified`, `agent`), and humans
//! (`human`). Every per-agent aggregate built on it is therefore unsound: the
//! same actor is split across three granularities, and synthetic buckets
//! outrank real agents.
//!
//! This module is the one place that turns a recorded label into a canonical
//! [`CanonicalActor`]: an [`ActorKind`] plus, for agents, separately
//! addressable vendor / family / model fields. Persistence materializes the
//! result into columns so aggregate SQL can `GROUP BY` the canonical actor;
//! adding a new model means editing [`canonical_actor_for_role_label`], never
//! an aggregate query.
//!
//! ## Trust is not identity
//!
//! This module normalizes identity *shape* only. It never rewrites `role`, and
//! `unverified` maps to its own unattributed actor rather than being resolved
//! to whatever agent the label hints at. Trust classification reads `role` and
//! is unaffected by anything here.
//!
//! ## Alias-map versioning
//!
//! [`ACTOR_ALIAS_MAP_VERSION`] stamps every derived record. Re-running an
//! aggregate over old rows is stable because the rows carry the version that
//! produced them: a map change is a version bump plus a re-derivation step,
//! not a silent reinterpretation. See `docs/design/audit-actor-identity/`.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::identity::{agent_from_model, all_agent_families, provider_for_agent_family};

/// Version of the label → [`CanonicalActor`] alias map below.
///
/// Bump this whenever a label's canonical resolution changes (a new alias, a
/// re-kinded label, a corrected family). Adding a model that an existing rule
/// already resolves — any `claude-*` build, say — is not a map change and must
/// not bump it.
///
/// History: v2 moved `fable` from a shorthand alias to a family rule, so
/// versioned Fable labels (`fable-5.1`) resolve to `claude` instead of an
/// unrecognized family.
pub const ACTOR_ALIAS_MAP_VERSION: u32 = 2;

/// What kind of thing performed an audited invocation.
///
/// This is the dimension that makes "real agents only" expressible without
/// string-matching the label.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// A person driving Orbit directly.
    Human,
    /// An AI coding agent, identified by family and (when recorded) model.
    Agent,
    /// Orbit itself: ID allocation, internal bookkeeping, admin commands.
    System,
    /// Editor/CLI hook machinery, not a caller in its own right.
    Hook,
    /// No usable attribution was recorded. Kept as a first-class kind so it
    /// shows up as a measurable gap instead of masquerading as an agent.
    Unattributed,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::System => "system",
            ActorKind::Hook => "hook",
            ActorKind::Unattributed => "unattributed",
        }
    }
}

impl Display for ActorKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ActorKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "human" => Ok(ActorKind::Human),
            "agent" => Ok(ActorKind::Agent),
            "system" => Ok(ActorKind::System),
            "hook" => Ok(ActorKind::Hook),
            "unattributed" => Ok(ActorKind::Unattributed),
            other => Err(format!("unknown actor kind: {other}")),
        }
    }
}

/// A recorded audit label resolved into its canonical parts.
///
/// `id` is the grouping key: for agents it is the family, so `claude`,
/// `opus`, and `claude-opus-5` all collapse to `claude` while `model` keeps
/// the finer grain retrievable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalActor {
    pub kind: ActorKind,
    /// Stable grouping key within `kind`. Agent families for agents; the
    /// canonical label (`admin`, `hook`, `unknown`, …) otherwise.
    pub id: String,
    /// Provider that serves the model (`anthropic`, `openai`, …). `None` for
    /// non-agents and for agent labels whose family is unrecognized.
    pub vendor: Option<String>,
    /// Orbit agent family (`claude`, `codex`, `gemini`, `grok`, `ollama`).
    pub family: Option<String>,
    /// Model string as recorded, when the label named a model rather than a
    /// bare family. `None` when only the family was recorded.
    pub model: Option<String>,
    /// [`ACTOR_ALIAS_MAP_VERSION`] at derivation time.
    pub alias_version: u32,
}

impl CanonicalActor {
    fn new(
        kind: ActorKind,
        id: impl Into<String>,
        vendor: Option<String>,
        family: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            vendor,
            family,
            model,
            alias_version: ACTOR_ALIAS_MAP_VERSION,
        }
    }

    /// A non-agent actor whose grouping key is the label itself.
    fn tagged(kind: ActorKind, id: &str) -> Self {
        Self::new(kind, id, None, None, None)
    }

    /// True when this actor is a real agent, without inspecting any label.
    pub fn is_agent(&self) -> bool {
        self.kind == ActorKind::Agent
    }
}

/// The label Orbit records when no attribution at all was resolved.
const UNKNOWN_LABEL: &str = "unknown";

/// Labels that are recorded on the `role` column but do not name a caller.
///
/// Editing this table is an alias-map change: bump [`ACTOR_ALIAS_MAP_VERSION`].
const NON_AGENT_ALIASES: &[(&str, ActorKind)] = &[
    // `role: "admin"` is hardcoded on ID-allocation events and on every direct
    // CLI command's audit row; it is Orbit acting, not a distinct caller.
    ("admin", ActorKind::System),
    ("system", ActorKind::System),
    ("hook", ActorKind::Hook),
    ("human", ActorKind::Human),
    // Attribution was attempted and produced nothing usable. `unverified` is
    // the MCP trust boundary's marker and must keep its own identity.
    ("unknown", ActorKind::Unattributed),
    ("unverified", ActorKind::Unattributed),
    // The generic fallback `audit_role_label` emits when identity resolution
    // yields neither a family nor a model.
    ("agent", ActorKind::Unattributed),
];

/// Model shorthands Orbit itself emits that the model-string family rules do
/// not already cover.
///
/// Editing this table is an alias-map change: bump [`ACTOR_ALIAS_MAP_VERSION`].
const MODEL_SHORTHAND_ALIASES: &[(&str, &str)] = &[("haiku", "claude")];

/// Resolve a recorded `audit_events.role` label into its canonical actor.
///
/// Resolution order:
/// 1. Empty/blank → unattributed `unknown`.
/// 2. A known non-agent alias (`admin`, `hook`, `human`, `unknown`, …).
/// 3. A bare agent family name (`claude`) → agent with no model recorded.
/// 4. A model string whose family is inferable (`claude-opus-5`, `opus`,
///    `gpt-6-luna`) → agent with family *and* model.
/// 5. Anything else → agent with an unknown family, keeping the label as the
///    model. Every label reaching this point was produced by an attribution
///    path that had a model or family in hand, so treating it as an
///    unrecognized agent model loses less than discarding it would.
pub fn canonical_actor_for_role_label(label: &str) -> CanonicalActor {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return CanonicalActor::tagged(ActorKind::Unattributed, UNKNOWN_LABEL);
    }

    let lowered = trimmed.to_ascii_lowercase();

    if let Some((canonical, kind)) = NON_AGENT_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lowered)
    {
        return CanonicalActor::tagged(*kind, canonical);
    }

    if let Some(family) = all_agent_families()
        .iter()
        .find(|family| **family == lowered)
    {
        return agent_actor(family, None);
    }

    if let Some(family) = agent_from_model(&lowered) {
        return agent_actor(family, Some(trimmed));
    }

    if let Some((_, family)) = MODEL_SHORTHAND_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lowered)
    {
        return agent_actor(family, Some(trimmed));
    }

    // Unrecognized family: group by the model string so the actor is still a
    // single row rather than being folded into an unrelated bucket.
    CanonicalActor::new(
        ActorKind::Agent,
        trimmed,
        None,
        None,
        Some(trimmed.to_string()),
    )
}

fn agent_actor(family: &str, model: Option<&str>) -> CanonicalActor {
    CanonicalActor::new(
        ActorKind::Agent,
        family,
        provider_for_agent_family(family).map(ToOwned::to_owned),
        Some(family.to_string()),
        model.map(ToOwned::to_owned),
    )
}
