use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::IdentityError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentFamily {
    Codex,
    Claude,
    Gemini,
    Grok,
    Muse,
}

impl AgentFamily {
    pub const ALL: [Self; 5] = [
        Self::Codex,
        Self::Claude,
        Self::Gemini,
        Self::Grok,
        Self::Muse,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::Muse => "muse",
        }
    }

    pub fn parse(value: &str) -> Result<Self, IdentityError> {
        value.parse()
    }
}

impl Display for AgentFamily {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentFamily {
    type Err = IdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "gemini" => Ok(Self::Gemini),
            "grok" => Ok(Self::Grok),
            "muse" => Ok(Self::Muse),
            other => Err(IdentityError::Invalid(format!(
                "unknown agent family '{other}'; expected one of codex, claude, gemini, grok, muse"
            ))),
        }
    }
}

/// Map a lower-cased model identifier prefix to its agent family.
pub(crate) fn agent_family_from_model_prefix(model: &str) -> Option<AgentFamily> {
    if model.starts_with("claude-")
        || model.starts_with("opus")
        || model.starts_with("sonnet")
        || model.starts_with("fable")
    {
        return Some(AgentFamily::Claude);
    }
    if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        return Some(AgentFamily::Codex);
    }
    if model.starts_with("gemini-") {
        return Some(AgentFamily::Gemini);
    }
    // Grok (xAI) family — covers grok-4, grok-3, and grok3* short names
    if model.starts_with("grok-") || model.starts_with("grok3") {
        return Some(AgentFamily::Grok);
    }
    if model.starts_with("muse-") {
        return Some(AgentFamily::Muse);
    }

    None
}
