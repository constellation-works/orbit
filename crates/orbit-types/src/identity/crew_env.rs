//! One `[crews.<name>] env` entry: a variable a crew's agents receive on top
//! of `[execution.env] pass`.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::IdentityError;

/// Prefix that marks a crew `env` source as a file path rather than a
/// variable name. `:` can never appear in a variable name, so the two forms
/// cannot be mistaken for each other.
const CREW_ENV_FILE_PREFIX: &str = "file:";

/// Where a crew `env` entry's value comes from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrewEnvSource {
    /// The named variable of the launching process (or of `clock.env`).
    Variable(String),
    /// An owner-only file the host reads when it starts the agent, written
    /// `file:<absolute path or ~/path>`. Holds the path as written.
    File(String),
}

impl CrewEnvSource {
    /// The variable name, for a variable source.
    pub fn variable(&self) -> Option<&str> {
        match self {
            Self::Variable(name) => Some(name),
            Self::File(_) => None,
        }
    }

    /// The path as written, for a file source.
    pub fn file(&self) -> Option<&str> {
        match self {
            Self::File(path) => Some(path),
            Self::Variable(_) => None,
        }
    }
}

impl fmt::Display for CrewEnvSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variable(name) => f.write_str(name),
            Self::File(path) => write!(f, "{CREW_ENV_FILE_PREFIX}{path}"),
        }
    }
}

/// One `crews.<name>.env` entry, written `NAME`, `NAME=SOURCE` or
/// `NAME=file:PATH`.
///
/// The crew's agents receive `name` with the value the launching process
/// holds for the `source` variable, or the contents of the `source` file; a
/// bare `NAME` uses the same name on both sides. Only names and paths are
/// configured, never values. Serialized in its written form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CrewEnvVar {
    /// The variable the agent receives.
    pub name: String,
    /// Where its value comes from.
    pub source: CrewEnvSource,
}

impl CrewEnvVar {
    /// Parse one written entry. The agent variable, and a variable source,
    /// must be identifiers (`[A-Za-z_][A-Za-z0-9_]*`), and neither may be an
    /// `ORBIT_*` name: those are Orbit's own execution envelope or
    /// privilege-bearing, never a credential a crew forwards. A source written
    /// `file:<path>` names a file instead, which must be absolute or start
    /// with `~/`, since the launchers that read it have different working
    /// directories.
    pub fn parse(entry: &str) -> Result<Self, IdentityError> {
        let (name, source) = match entry.split_once('=') {
            Some((name, source)) => (name, source),
            None => (entry, entry),
        };
        if let Some(path) = source.strip_prefix(CREW_ENV_FILE_PREFIX) {
            if !(path.starts_with('/') || path.starts_with("~/")) || path.ends_with('/') {
                return Err(IdentityError::Invalid(format!(
                    "'{entry}' is not NAME=file:PATH: the path must be absolute or start with \
                     '~/' and name a file"
                )));
            }
            check_variable_name(entry, name)?;
            return Ok(Self {
                name: name.to_string(),
                source: CrewEnvSource::File(path.to_string()),
            });
        }
        for side in [name, source] {
            check_variable_name(entry, side)?;
        }
        Ok(Self {
            name: name.to_string(),
            source: CrewEnvSource::Variable(source.to_string()),
        })
    }

    /// Whether the agent's variable takes its value from a differently named
    /// variable or from a file.
    pub fn is_mapped(&self) -> bool {
        match &self.source {
            CrewEnvSource::Variable(source) => &self.name != source,
            CrewEnvSource::File(_) => true,
        }
    }
}

fn check_variable_name(entry: &str, side: &str) -> Result<(), IdentityError> {
    if !is_identifier(side) {
        return Err(IdentityError::Invalid(format!(
            "'{entry}' is not NAME, NAME=SOURCE or NAME=file:PATH: '{side}' is not a variable \
             name (letters, digits and '_', not starting with a digit)"
        )));
    }
    if side.starts_with("ORBIT_") {
        return Err(IdentityError::Invalid(format!(
            "'{entry}' names '{side}': ORBIT_* variables are Orbit's own and cannot be \
             forwarded by a crew"
        )));
    }
    Ok(())
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
