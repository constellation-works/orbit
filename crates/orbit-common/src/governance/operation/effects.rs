use crate::OrbitError;

/// Whether an operation reads or writes durable state.
///
/// One declaration that every surface derives its runtime wiring from: the
/// tool's execution kind and the CLI's default runtime need. A guard that
/// depends on the input, such as a write permission needed only when an update
/// moves a record, stays in the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Observes state without changing it.
    Read,
    /// May change durable state.
    Write,
}

/// A field an operation used to accept, or never accepted, and refuses by name.
#[derive(Debug, Clone, Copy)]
pub struct RetiredField {
    /// The refused wire field name.
    pub name: &'static str,
    /// What to send instead; the refusal ends with it, or is it, per
    /// [`Self::wording`].
    pub replacement: &'static str,
    /// How the refusal is phrased. Shipped refusals keep their wording.
    pub wording: RetiredWording,
}

/// The phrasing of a [`RetiredField`] refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetiredWording {
    /// ``{tool} no longer accepts `{name}`; {replacement}``.
    NoLongerAccepts,
    /// `unknown field '{name}' ({tool} does not accept '{name}'; {replacement})`.
    UnknownField,
    /// The replacement is the whole refusal.
    Verbatim,
}

impl RetiredField {
    /// A field the operation no longer accepts, refused with its replacement.
    pub const fn no_longer_accepts(name: &'static str, replacement: &'static str) -> Self {
        Self {
            name,
            replacement,
            wording: RetiredWording::NoLongerAccepts,
        }
    }

    /// A field refused as unknown to this operation, naming where it belongs.
    pub const fn unknown_field(name: &'static str, replacement: &'static str) -> Self {
        Self {
            name,
            replacement,
            wording: RetiredWording::UnknownField,
        }
    }

    /// A field refused with a message of its own.
    pub const fn verbatim(name: &'static str, refusal: &'static str) -> Self {
        Self {
            name,
            replacement: refusal,
            wording: RetiredWording::Verbatim,
        }
    }

    pub(super) fn refusal(&self, tool_name: &str) -> OrbitError {
        let name = self.name;
        let replacement = self.replacement;
        OrbitError::InvalidInput(match self.wording {
            RetiredWording::NoLongerAccepts => {
                format!("{tool_name} no longer accepts `{name}`; {replacement}")
            }
            RetiredWording::UnknownField => format!(
                "unknown field '{name}' ({tool_name} does not accept '{name}'; {replacement})"
            ),
            RetiredWording::Verbatim => replacement.to_string(),
        })
    }
}

/// The legacy top-level `agent` field: attribution was consolidated to
/// `model`-only.
pub const RETIRED_AGENT_FIELD: RetiredField =
    RetiredField::no_longer_accepts("agent", "use `model` with the agent family for attribution");

/// What an operation does with an input field it does not declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownFields {
    /// Report the field and proceed: the compatibility window for an
    /// operation that used to ignore extras.
    Warn,
    /// Refuse the call, suggesting a declared field when one is close: an
    /// operation that already refused extras before it migrated.
    Refuse,
}
