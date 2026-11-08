//! The values a registry key accepts beyond its TOML type [ORB-22809].
//!
//! Each [`super::ConfigKeyDescriptor`] carries one [`ConfigAllowedValues`].
//! `orbit config keys`, `orbit config show`, the config API and the
//! dashboard's Settings rows show it, and the shared resolvers
//! (`resolve_bounded_range`, `resolve_choice` and their wrappers) read their
//! bounds and choices from it by key. The values a view shows are therefore
//! the values config load and `orbit config set` accept.

use serde_json::{Value as JsonValue, json};

/// What a key accepts beyond its value type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfigAllowedValues {
    /// Any value of the key's type: a bool, or a value only its own
    /// resolver constrains.
    Any,
    /// Exactly one of these strings.
    OneOf(&'static [&'static str]),
    /// A list whose every entry is one of these strings.
    EachOf(&'static [&'static str]),
    /// A whole number in `min..=max`; `max` is `None` when only the type
    /// bounds it.
    Integer {
        /// Smallest accepted value.
        min: u64,
        /// Largest accepted value, when the key bounds it.
        max: Option<u64>,
        /// What the number counts (`minutes`, `%`, `bytes`, ...).
        unit: Option<&'static str>,
    },
    /// A finite number in `min..=max`.
    Number {
        /// Smallest accepted value.
        min: f64,
        /// Largest accepted value, when the key bounds it.
        max: Option<f64>,
        /// What the number measures.
        unit: Option<&'static str>,
    },
    /// A free-form value; the note names its shape.
    Format(&'static str),
}

/// `value` must be one of `choices`.
pub(crate) const fn one_of(choices: &'static [&'static str]) -> ConfigAllowedValues {
    ConfigAllowedValues::OneOf(choices)
}

/// Every list entry must be one of `choices`.
pub(crate) const fn each_of(choices: &'static [&'static str]) -> ConfigAllowedValues {
    ConfigAllowedValues::EachOf(choices)
}

/// A whole number in `min..=max`, counting `unit`.
pub(crate) const fn integers(min: u64, max: u64, unit: &'static str) -> ConfigAllowedValues {
    ConfigAllowedValues::Integer {
        min,
        max: Some(max),
        unit: if unit.is_empty() { None } else { Some(unit) },
    }
}

/// A whole number of at least `min`, counting `unit`.
pub(crate) const fn integers_from(min: u64, unit: &'static str) -> ConfigAllowedValues {
    ConfigAllowedValues::Integer {
        min,
        max: None,
        unit: if unit.is_empty() { None } else { Some(unit) },
    }
}

/// A finite number in `min..=max`.
pub(crate) const fn numbers(min: f64, max: f64) -> ConfigAllowedValues {
    ConfigAllowedValues::Number {
        min,
        max: Some(max),
        unit: None,
    }
}

/// A free-form value shaped as `note` says.
pub(crate) const fn format(note: &'static str) -> ConfigAllowedValues {
    ConfigAllowedValues::Format(note)
}

/// Percentages, `1..=100`.
pub(crate) const PERCENT: ConfigAllowedValues = integers(1, 100, "%");

impl ConfigAllowedValues {
    /// The choices of a [`Self::OneOf`] key, or `None`.
    #[must_use]
    pub fn choices(&self) -> Option<&'static [&'static str]> {
        match self {
            Self::OneOf(choices) => Some(choices),
            _ => None,
        }
    }

    /// The JSON shape views read, or `null` for [`Self::Any`]:
    /// `{"kind": "enum", "values": [...]}`, `{"kind": "enum_list", "values":
    /// [...]}`, `{"kind": "range", "integer": bool, "min", "max", "unit"}`
    /// (`max` and `unit` may be `null`), or `{"kind": "format", "note"}`.
    /// Each carries `summary`, the one-line form [`Self::summary`] renders.
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        let mut value = match *self {
            Self::Any => return JsonValue::Null,
            Self::OneOf(values) => json!({ "kind": "enum", "values": values }),
            Self::EachOf(values) => json!({ "kind": "enum_list", "values": values }),
            Self::Integer { min, max, unit } => json!({
                "kind": "range", "integer": true, "min": min, "max": max, "unit": unit,
            }),
            Self::Number { min, max, unit } => json!({
                "kind": "range", "integer": false, "min": min, "max": max, "unit": unit,
            }),
            Self::Format(note) => json!({ "kind": "format", "note": note }),
        };
        value["summary"] = json!(self.summary());
        value
    }

    /// One line for a table cell: `off | shadow | enforce`, `each of: a | b`,
    /// `1–1440 minutes`, `≥ 1`, or the format note. `None` for [`Self::Any`].
    #[must_use]
    pub fn summary(&self) -> Option<String> {
        let with_unit = |range: String, unit: Option<&str>| match unit {
            Some("%") => format!("{range}%"),
            Some(unit) => format!("{range} {unit}"),
            None => range,
        };
        match *self {
            Self::Any => None,
            Self::OneOf(values) => Some(values.join(" | ")),
            Self::EachOf(values) => Some(format!("each of: {}", values.join(" | "))),
            Self::Integer { min, max, unit } => Some(with_unit(
                match max {
                    Some(max) => format!("{min}–{max}"),
                    None => format!("≥ {min}"),
                },
                unit,
            )),
            Self::Number { min, max, unit } => Some(with_unit(
                match max {
                    Some(max) => format!("{min}–{max}"),
                    None => format!("≥ {min}"),
                },
                unit,
            )),
            Self::Format(note) => Some(note.to_string()),
        }
    }
}
