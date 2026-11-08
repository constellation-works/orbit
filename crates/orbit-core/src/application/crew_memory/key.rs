//! Memory keys: the model a space belongs to, or `global`.

use std::fmt;

use orbit_common::OrbitError;
use serde::Serialize;

/// The key of the memory every model shares.
pub const GLOBAL_MEMORY_KEY: &str = "global";

/// Longest accepted model id.
const MAX_KEY_LEN: usize = 128;

/// A model id, as runs record it, or `global`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MemoryKey(String);

impl MemoryKey {
    /// The global memory's key.
    pub fn global() -> Self {
        Self(GLOBAL_MEMORY_KEY.to_string())
    }

    pub fn is_global(&self) -> bool {
        self.0 == GLOBAL_MEMORY_KEY
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The directory `orbit memory export` renders a model's files into: the
    /// key with `/` and `:` replaced by `_`, so every key is one path segment.
    pub fn export_dir_name(&self) -> String {
        self.0.replace(['/', ':'], "_")
    }
}

impl fmt::Display for MemoryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The memory key of a run, from its recorded `crew_model`. A run with no
/// recorded model, or a blank, malformed or `unknown` one, has no key and
/// gets no memory: the key is never guessed from a crew name or provider.
pub fn memory_key(recorded_crew_model: Option<&str>) -> Option<MemoryKey> {
    let model = recorded_crew_model?.trim();
    let valid = !model.is_empty()
        && model.len() <= MAX_KEY_LEN
        && model.starts_with(|c: char| c.is_ascii_alphanumeric())
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:/@+".contains(c))
        && !model
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty());
    (valid && model != GLOBAL_MEMORY_KEY && !model.eq_ignore_ascii_case("unknown"))
        .then(|| MemoryKey(model.to_string()))
}

/// An operator-named key: `global`, or a model id under the same rule as a
/// run's recorded model.
pub fn parse_memory_key(raw: &str) -> Result<MemoryKey, OrbitError> {
    let raw = raw.trim();
    if raw == GLOBAL_MEMORY_KEY {
        return Ok(MemoryKey::global());
    }
    memory_key(Some(raw)).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "`{raw}` is not a memory key: name a model id as runs record it (letters, digits and . _ - : / @ +, at most {MAX_KEY_LEN} characters) or `global`"
        ))
    })
}
