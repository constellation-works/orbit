//! Capability labels: which hosts may run a task, beyond its `os:` tags.
//!
//! A machine declares opaque operator labels in `machine.labels`
//! (`["xcode", "gpu"]`), and a task names the ones it needs in
//! `requires_labels`. The host's OS is an implicit label (`linux`, `macos`,
//! `windows`), so a task that needs macOS and Xcode can say so in one place.
//! A host may start a task only when its labels are a superset of the task's:
//! the requirement is all-of, unlike `os:` tags, which stay any-of.
//!
//! Workspace tag mappings can add requirements without persisting them on a task.
//! Orbit gives no label a meaning beyond the implicit OS one. Labels compare
//! after trimming and lowercasing, and both sides are normalized the same way
//! at write, so `GPU` and `gpu` are the same label.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::task::{HostOs, TaskError};

/// The longest label accepted, in bytes.
pub const MAX_HOST_LABEL_BYTES: usize = 64;

/// The task field that names the labels a host must carry.
pub const REQUIRES_LABELS_FIELD: &str = "requires_labels";

/// Trim, lowercase, sort and deduplicate `labels`, refusing a blank label, one
/// longer than [`MAX_HOST_LABEL_BYTES`], or one with a character outside ASCII
/// letters, digits, `-`, `_`, `.`, `:` and `/`. `field` names the setting or
/// task field in the error.
pub fn normalize_host_labels(labels: Vec<String>, field: &str) -> Result<Vec<String>, TaskError> {
    let mut normalized = BTreeSet::new();
    for label in labels {
        let label = label.trim().to_ascii_lowercase();
        if label.is_empty() {
            return Err(TaskError::Invalid(format!(
                "{field} contains a blank label"
            )));
        }
        if label.len() > MAX_HOST_LABEL_BYTES {
            return Err(TaskError::Invalid(format!(
                "{field} label `{label}` is longer than {MAX_HOST_LABEL_BYTES} bytes"
            )));
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/'))
        {
            return Err(TaskError::Invalid(format!(
                "{field} label `{label}` may use only ASCII letters, digits, `-`, `_`, `.`, `:` \
                 and `/`"
            )));
        }
        normalized.insert(label);
    }
    Ok(normalized.into_iter().collect())
}

/// A host's effective label set: its configured labels plus its OS as an
/// implicit label. `configured` is already normalized.
#[must_use]
pub fn effective_host_labels(configured: &[String], os: Option<HostOs>) -> BTreeSet<String> {
    let mut labels = configured.iter().cloned().collect::<BTreeSet<_>>();
    labels.extend(os.map(|os| os.as_str().to_string()));
    labels
}

/// The labels in `required` that `host` lacks, in order. Empty when `host`
/// may start the task.
#[must_use]
pub fn missing_host_labels(required: &[String], host: &BTreeSet<String>) -> Vec<String> {
    required
        .iter()
        .filter(|label| !host.contains(label.as_str()))
        .cloned()
        .collect()
}

/// `waits for a host labelled `gpu`, `xcode` (requires_labels)`.
#[must_use]
pub fn host_labels_wait(required: &[String]) -> String {
    format!(
        "waits for a host labelled {} ({REQUIRES_LABELS_FIELD})",
        quoted_labels(required)
    )
}

/// Why a host carrying `host` may not start a task requiring `required`, or
/// `None` when it may: the wait, and which labels the host lacks.
#[must_use]
pub fn host_labels_unavailable(required: &[String], host: &BTreeSet<String>) -> Option<String> {
    let missing = missing_host_labels(required, host);
    (!missing.is_empty()).then(|| {
        format!(
            "{}; the executor lacks {}",
            host_labels_wait(required),
            quoted_labels(&missing)
        )
    })
}

/// `` `gpu`, `xcode` ``, or `none` for an empty set.
#[must_use]
pub fn quoted_labels<S: AsRef<str>>(labels: impl IntoIterator<Item = S>) -> String {
    let quoted = labels
        .into_iter()
        .map(|label| format!("`{}`", label.as_ref()))
        .collect::<Vec<_>>();
    if quoted.is_empty() {
        "none".to_string()
    } else {
        quoted.join(", ")
    }
}

/// Configuration key mapping task tags to additional required capabilities.
pub const TAG_REQUIRES_LABELS_KEY: &str = "workflow.tag_requires_labels";

/// Validated tag-to-capability policy. No tag or label has built-in placement
/// meaning here; every matching tag adds requirements to the explicit set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<String, Vec<String>>",
    into = "BTreeMap<String, Vec<String>>"
)]
pub struct TagLabelRequirements(BTreeMap<String, Vec<String>>);

impl TryFrom<BTreeMap<String, Vec<String>>> for TagLabelRequirements {
    type Error = TaskError;

    fn try_from(raw: BTreeMap<String, Vec<String>>) -> Result<Self, Self::Error> {
        let mut mappings = BTreeMap::new();
        for (raw_tag, labels) in raw {
            let tag = crate::workflow::normalize_instruction_tag(&raw_tag).ok_or_else(|| {
                TaskError::Invalid(format!("{TAG_REQUIRES_LABELS_KEY} contains a blank tag"))
            })?;
            let labels =
                normalize_host_labels(labels, &format!("{TAG_REQUIRES_LABELS_KEY}.{tag}"))?;
            if mappings.insert(tag.clone(), labels).is_some() {
                return Err(TaskError::Invalid(format!(
                    "{TAG_REQUIRES_LABELS_KEY} maps tag `{tag}` more than once; tags match case-insensitively"
                )));
            }
        }
        Ok(Self(mappings))
    }
}

impl TagLabelRequirements {
    /// Sorted, deduplicated union of explicit labels and all matching tags'
    /// requirements. Explicit labels have already been normalized at write.
    #[must_use]
    pub fn required_labels(&self, tags: &[String], explicit: &[String]) -> Vec<String> {
        if self.0.is_empty() {
            return explicit.to_vec();
        }
        let mut required = explicit.iter().cloned().collect::<BTreeSet<_>>();
        for tag in tags {
            if let Some(labels) = self.0.get(&tag.trim().to_lowercase()) {
                required.extend(labels.iter().cloned());
            }
        }
        required.into_iter().collect()
    }
}

impl From<TagLabelRequirements> for BTreeMap<String, Vec<String>> {
    fn from(requirements: TagLabelRequirements) -> Self {
        requirements.0
    }
}
