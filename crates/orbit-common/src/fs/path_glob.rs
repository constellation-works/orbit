//! Confined relative path globs, shared by configuration and filesystem consumers.

use crate::OrbitError;

/// A relative path pattern: `*` stays in a component and `**` spans components.
#[derive(Debug, Clone)]
pub struct RelativePathGlob {
    components: Vec<String>,
}

impl RelativePathGlob {
    /// Admit a pattern that cannot name an absolute path, parent or root.
    /// Globstars must occupy a whole component; other characters are literal.
    pub fn new(pattern: &str) -> Result<Self, OrbitError> {
        let components = pattern.split('/').map(str::to_owned).collect::<Vec<_>>();
        if pattern.contains(['\\', ':', '\0'])
            || components.iter().any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || (part.contains("**") && part != "**")
            })
            || components.iter().all(|part| part == "**")
        {
            return Err(OrbitError::InvalidInput(format!(
                "invalid relative path glob '{pattern}': use a non-root relative path without '..'; '**' must be a whole component"
            )));
        }
        Ok(Self { components })
    }

    /// Match a slash-separated relative path, including dot-prefixed components.
    pub fn matches(&self, path: &str) -> bool {
        let parts = if path.is_empty() {
            Vec::new()
        } else {
            path.split('/').collect()
        };
        let mut states = vec![false; parts.len() + 1];
        states[0] = true;
        for pattern in &self.components {
            let mut next = vec![false; states.len()];
            for index in 0..states.len() {
                if pattern == "**" {
                    next[index] = states[index] || (index > 0 && next[index - 1]);
                } else if index > 0 {
                    next[index] = states[index - 1] && component_matches(pattern, parts[index - 1]);
                }
            }
            states = next;
        }
        states[parts.len()]
    }

    /// Whether a strict descendant of this path could match, for bounded traversal.
    pub fn may_match_descendant(&self, path: &str) -> bool {
        let parts = if path.is_empty() {
            Vec::new()
        } else {
            path.split('/').collect()
        };
        let mut states = vec![false; self.components.len() + 1];
        states[0] = true;
        for part in parts {
            for index in 0..self.components.len() {
                if self.components[index] == "**" && states[index] {
                    states[index + 1] = true;
                }
            }
            let mut next = vec![false; states.len()];
            for (index, pattern) in self.components.iter().enumerate() {
                if pattern == "**" {
                    next[index] |= states[index];
                } else {
                    next[index + 1] |= states[index] && component_matches(pattern, part);
                }
            }
            states = next;
        }
        states[..self.components.len()].iter().any(|state| *state)
    }
}

fn component_matches(pattern: &str, value: &str) -> bool {
    let mut states = vec![false; value.len() + 1];
    states[0] = true;
    for byte in pattern.bytes() {
        let mut next = vec![false; states.len()];
        for index in 0..states.len() {
            next[index] = if byte == b'*' {
                states[index] || (index > 0 && next[index - 1])
            } else {
                index > 0 && states[index - 1] && value.as_bytes()[index - 1] == byte
            };
        }
        states = next;
    }
    states[value.len()]
}
