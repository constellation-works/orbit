//! Shared deterministic duplicate-task assessment for finding sweep actions.
//!
//! Exact generated-key tags remain the authoritative fast path. When that
//! path misses, a candidate is compared with every still-open task using
//! caller-supplied, high-confidence fingerprints. A fingerprint must match in
//! full; individual keywords never carry a duplicate decision.

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

const MAX_EVIDENCE_FIELDS: usize = 6;
const MAX_EVIDENCE_CHARS: usize = 160;

pub(in crate::adapter::engine_host::v2_host) trait DuplicateTaskLookup {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError>;

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError>;
}

impl DuplicateTaskLookup for crate::OrbitRuntime {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        crate::OrbitRuntime::list_tasks_by_tags(self, tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        crate::OrbitRuntime::list_tasks(self)
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct DuplicateCandidate {
    exact_tag: String,
    fingerprints: Vec<CoverageFingerprint>,
}

impl DuplicateCandidate {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        exact_tag: String,
        fingerprints: Vec<CoverageFingerprint>,
    ) -> Self {
        Self {
            exact_tag,
            fingerprints,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct CoverageFingerprint {
    name: &'static str,
    anchors: Vec<CoverageAnchor>,
}

impl CoverageFingerprint {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        name: &'static str,
        anchors: Vec<CoverageAnchor>,
    ) -> Self {
        Self { name, anchors }
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct CoverageAnchor {
    field: &'static str,
    value: String,
}

impl CoverageAnchor {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        field: &'static str,
        value: impl Into<String>,
    ) -> Self {
        Self {
            field,
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct DuplicateTaskMatch {
    pub task_id: String,
    pub match_kind: &'static str,
    pub evidence: Value,
}

/// Find a still-open task covering `candidate`.
///
/// The tag query deliberately happens before the broader task listing. An
/// exact generated key is deterministic and sufficient by itself, so a broad
/// lookup is neither needed nor allowed to override it.
pub(in crate::adapter::engine_host::v2_host) fn find_covering_task<L>(
    lookup: &L,
    candidate: &DuplicateCandidate,
) -> Result<Option<DuplicateTaskMatch>, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    let exact_tasks = lookup.list_tasks_by_tags(std::slice::from_ref(&candidate.exact_tag))?;
    if let Some(task) = exact_tasks
        .into_iter()
        .find(|task| is_open_status(task.status))
    {
        return Ok(Some(DuplicateTaskMatch {
            task_id: task.id,
            match_kind: "exact_key",
            evidence: bounded_evidence(
                "generated_key",
                &[CoverageAnchor::new("dedupe_tag", &candidate.exact_tag)],
            ),
        }));
    }

    validate_candidate(candidate)?;
    let mut open_tasks = lookup
        .list_tasks()?
        .into_iter()
        .filter(|task| is_open_status(task.status))
        .collect::<Vec<_>>();
    open_tasks.sort_by(|left, right| left.id.cmp(&right.id));

    for task in open_tasks {
        let searchable = searchable_task_text(&task);
        if let Some(fingerprint) = candidate
            .fingerprints
            .iter()
            .find(|fingerprint| fingerprint_matches(&searchable, fingerprint))
        {
            return Ok(Some(DuplicateTaskMatch {
                task_id: task.id,
                match_kind: "material_coverage",
                evidence: bounded_evidence(fingerprint.name, &fingerprint.anchors),
            }));
        }
    }

    Ok(None)
}

fn validate_candidate(candidate: &DuplicateCandidate) -> Result<(), OrbitError> {
    if candidate.exact_tag.trim().is_empty()
        || candidate.fingerprints.is_empty()
        || candidate
            .fingerprints
            .iter()
            .any(|fingerprint| fingerprint.anchors.is_empty())
        || candidate
            .fingerprints
            .iter()
            .flat_map(|fingerprint| &fingerprint.anchors)
            .any(|anchor| canonical_text(&anchor.value).trim().is_empty())
    {
        return Err(OrbitError::InvalidInput(
            "duplicate-task candidate has an incomplete coverage fingerprint".to_string(),
        ));
    }
    Ok(())
}

fn fingerprint_matches(searchable: &str, fingerprint: &CoverageFingerprint) -> bool {
    fingerprint.anchors.iter().all(|anchor| {
        let needle = canonical_text(&anchor.value);
        searchable.contains(&needle)
    })
}

fn searchable_task_text(task: &Task) -> String {
    let mut text = String::new();
    for value in std::iter::once(task.title.as_str())
        .chain(std::iter::once(task.description.as_str()))
        .chain(task.acceptance_criteria.iter().map(String::as_str))
        .chain(std::iter::once(task.plan.as_str()))
        .chain(task.tags.iter().map(String::as_str))
        .chain(task.context_files.iter().map(String::as_str))
    {
        text.push(' ');
        text.push_str(value);
    }
    canonical_text(&text)
}

/// Lowercase text into a token sequence with a leading and trailing space.
/// Searching canonical anchors in canonical task text therefore preserves
/// token boundaries (`time` cannot match `runtime`) while tolerating normal
/// prose and Markdown punctuation differences.
fn canonical_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len().saturating_add(2));
    out.push(' ');
    let mut separated = true;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            out.push(character);
            separated = false;
        } else if !separated {
            out.push(' ');
            separated = true;
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

fn bounded_evidence(kind: &str, anchors: &[CoverageAnchor]) -> Value {
    let fields = anchors
        .iter()
        .take(MAX_EVIDENCE_FIELDS)
        .map(|anchor| {
            json!({
                "field": anchor.field,
                "value": bounded_redacted(&anchor.value),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "fingerprint": kind,
        "matched_fields": fields,
    })
}

fn bounded_redacted(value: &str) -> String {
    redact_all(value).chars().take(MAX_EVIDENCE_CHARS).collect()
}

pub(in crate::adapter::engine_host::v2_host) fn is_open_status(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
    )
}

#[cfg(test)]
mod tests {
    use super::canonical_text;

    #[test]
    fn canonical_text_preserves_token_boundaries() {
        let searchable = canonical_text("Update runtime in Cargo.lock");
        assert!(!searchable.contains(&canonical_text("time")));
        assert!(searchable.contains(&canonical_text("runtime")));
    }
}
