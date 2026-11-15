//! `workflow.regression_layers`: the ordered path classifier that gives a
//! regression fix its layer in codebase regression analytics. Each row names
//! a layer and the path rules that put a path in it; the first matching row
//! wins and the row order is the order breakdowns list layers in. Unset, the
//! repository-neutral defaults apply: `ci_tests` and `docs_skills`.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

/// The key as written in `config.toml`.
const REGRESSION_LAYERS_KEY: &str = "workflow.regression_layers";

/// Layer names the analytics reserve: `other` for a path no row matches,
/// `unknown` for a fix with no path at all.
const RESERVED_REGRESSION_LAYERS: [&str; 2] = ["other", "unknown"];

/// One `[[workflow.regression_layers]]` entry: a layer name and the path
/// rules that put a repository-relative path in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegressionLayerDefinition {
    /// The layer's name; unique across the list and never `other` or
    /// `unknown`.
    #[serde(default)]
    pub name: String,
    /// A path starting with any of these texts is in the layer.
    #[serde(default)]
    pub prefixes: Vec<String>,
    /// A path with a directory component exactly equal to any of these
    /// names is in the layer.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// A path ending with any of these texts is in the layer.
    #[serde(default)]
    pub suffixes: Vec<String>,
}

impl RegressionLayerDefinition {
    /// Whether `path` matches any of this row's rules. Every component but
    /// the last names a directory; a directory path ends in `/`, so its own
    /// name counts too.
    #[must_use]
    pub fn matches(&self, path: &str) -> bool {
        self.prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix.as_str()))
            || self
                .suffixes
                .iter()
                .any(|suffix| path.ends_with(suffix.as_str()))
            || (!self.dirs.is_empty() && {
                let mut components: Vec<&str> = path.split('/').collect();
                components.pop();
                components
                    .iter()
                    .any(|component| self.dirs.iter().any(|dir| dir == component))
            })
    }
}

/// The rows an unset key resolves to: tests and CI, and documentation, by
/// layout conventions that hold in any repository.
#[must_use]
fn default_regression_layers() -> Vec<RegressionLayerDefinition> {
    let strings = |values: &[&str]| values.iter().map(|value| (*value).to_string()).collect();
    vec![
        RegressionLayerDefinition {
            name: "ci_tests".into(),
            prefixes: strings(&[".github/"]),
            dirs: strings(&["tests", "testdata", "fixtures"]),
            suffixes: Vec::new(),
        },
        RegressionLayerDefinition {
            name: "docs_skills".into(),
            prefixes: strings(&["docs/"]),
            dirs: Vec::new(),
            suffixes: strings(&[".md"]),
        },
    ]
}

/// Admit `workflow.regression_layers`: ordered entries with unique non-empty
/// names, `other` and `unknown` reserved, each holding at least one rule,
/// and every `dirs` entry a single path component. Unset keeps the
/// defaults; an empty list puts every path in `other`.
pub(crate) fn admit_regression_layers(
    raw: Option<Vec<RegressionLayerDefinition>>,
) -> Result<Vec<RegressionLayerDefinition>, OrbitError> {
    let Some(raw) = raw else {
        return Ok(default_regression_layers());
    };
    let invalid =
        |reason: String| OrbitError::InvalidInput(format!("{REGRESSION_LAYERS_KEY} {reason}"));
    let trimmed = |values: &[String]| -> Vec<String> {
        values
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect()
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut admitted = Vec::with_capacity(raw.len());
    for (index, entry) in raw.into_iter().enumerate() {
        let name = entry.name.trim().to_string();
        if name.is_empty() {
            return Err(invalid(format!("entry {} has an empty name", index + 1)));
        }
        let label = format!("entry `{name}`");
        if RESERVED_REGRESSION_LAYERS.contains(&name.as_str()) {
            return Err(invalid(format!("{label} uses the reserved name `{name}`")));
        }
        if !seen.insert(name.clone()) {
            return Err(invalid(format!("{label} repeats the name `{name}`")));
        }
        let prefixes = trimmed(&entry.prefixes);
        let dirs = trimmed(&entry.dirs);
        let suffixes = trimmed(&entry.suffixes);
        if prefixes.is_empty() && dirs.is_empty() && suffixes.is_empty() {
            return Err(invalid(format!(
                "{label} needs at least one of `prefixes`, `dirs` or `suffixes`"
            )));
        }
        if let Some(dir) = dirs.iter().find(|dir| dir.contains('/')) {
            return Err(invalid(format!(
                "{label} has dirs entry `{dir}`, which must be one path component"
            )));
        }
        admitted.push(RegressionLayerDefinition {
            name,
            prefixes,
            dirs,
            suffixes,
        });
    }
    Ok(admitted)
}
