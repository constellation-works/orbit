use thiserror::Error;

use super::ReasoningEffort;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum IdentityError {
    #[error("{0}")]
    Invalid(String),
    #[error("{message}")]
    InvalidWithSuggestions {
        message: String,
        did_you_mean: Vec<String>,
    },
}

impl IdentityError {
    pub fn invalid_input_with_suggestions(
        message: impl Into<String>,
        did_you_mean: Vec<String>,
    ) -> Self {
        if did_you_mean.is_empty() {
            Self::Invalid(message.into())
        } else {
            Self::InvalidWithSuggestions {
                message: message.into(),
                did_you_mean,
            }
        }
    }
}

/// A reasoning effort or model a provider CLI does not accept.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProviderModelError {
    #[error("provider '{provider}' does not support configured reasoning effort")]
    EffortUnsupportedByProvider { provider: String },
    #[error(
        "OpenCode CLI supports effort values high, max (`opencode run --variant`); '{effort}' is \
         unsupported. Values are not remapped; choose a supported effort or set a model whose \
         provider defines the variant."
    )]
    OpenCodeEffort { effort: ReasoningEffort },
    /// `model` is a verified Grok model whose documented efforts, listed in
    /// `supported`, exclude `effort`.
    #[error("Grok model '{model}' supports effort values {supported}; '{effort}' is unsupported")]
    GrokModelEffort {
        model: &'static str,
        supported: &'static str,
        effort: ReasoningEffort,
    },
    #[error(
        "Grok effort support is verified only for models 'grok-4.5', 'grok-4.6', and \
         'grok-4.7'; model '{model}' is unsupported"
    )]
    GrokModelUnverified { model: String },
    #[error(
        "Grok effort requires an explicit model; supported models are 'grok-4.5', 'grok-4.6', \
         and 'grok-4.7'"
    )]
    GrokModelMissing,
    #[error(
        "Antigravity CLI supports effort values low, medium, high (`agy --effort`); '{effort}' is \
         unsupported. Migrate xhigh/max to high, or choose a *-high model slug from `agy \
         models`. Values are not remapped."
    )]
    AntigravityEffort { effort: ReasoningEffort },
    /// `model` is a Gemini CLI id without the effort suffix `agy` requires.
    #[error(
        "Antigravity CLI does not accept Gemini CLI model id '{model}'. Use a slug from `agy \
         models` such as gemini-3.8-flash-high; ids are not remapped. Individual Gemini CLI \
         accounts stopped on 2026-06-18, but enterprise Gemini Code Assist and API-key Gemini \
         CLI remain available on the legacy `gemini` provider."
    )]
    LegacyGeminiModel { model: String },
}
