//! One `[crews.<name>] env` entry: a variable a crew's agents receive on top
//! of `[execution.env] pass`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::IdentityError;

/// One `crews.<name>.env` entry, written `NAME` or `NAME=SOURCE`.
///
/// The crew's agents receive `name` with the value the launching process
/// holds for `source`; a bare `NAME` uses the same name on both sides. Only
/// names are configured, never values. Serialized in its written form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CrewEnvVar {
    /// The variable the agent receives.
    pub name: String,
    /// The launching-process variable its value comes from.
    pub source: String,
}

impl CrewEnvVar {
    /// Parse one written entry. Both sides must be identifiers
    /// (`[A-Za-z_][A-Za-z0-9_]*`), and neither may be an `ORBIT_*` name: those
    /// are Orbit's own execution envelope or privilege-bearing, never a
    /// credential a crew forwards.
    pub fn parse(entry: &str) -> Result<Self, IdentityError> {
        let (name, source) = match entry.split_once('=') {
            Some((name, source)) => (name, source),
            None => (entry, entry),
        };
        for side in [name, source] {
            if !is_identifier(side) {
                return Err(IdentityError::Invalid(format!(
                    "'{entry}' is not NAME or NAME=SOURCE: '{side}' is not a variable name \
                     (letters, digits and '_', not starting with a digit)"
                )));
            }
            if side.starts_with("ORBIT_") {
                return Err(IdentityError::Invalid(format!(
                    "'{entry}' names '{side}': ORBIT_* variables are Orbit's own and cannot be \
                     forwarded by a crew"
                )));
            }
        }
        Ok(Self {
            name: name.to_string(),
            source: source.to_string(),
        })
    }

    /// Whether the agent's variable takes its value from a differently named
    /// one.
    pub fn is_mapped(&self) -> bool {
        self.name != self.source
    }
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_')
}

impl fmt::Display for CrewEnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_mapped() {
            write!(f, "{}={}", self.name, self.source)
        } else {
            f.write_str(&self.name)
        }
    }
}

impl TryFrom<String> for CrewEnvVar {
    type Error = IdentityError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CrewEnvVar> for String {
    fn from(value: CrewEnvVar) -> Self {
        value.to_string()
    }
}
