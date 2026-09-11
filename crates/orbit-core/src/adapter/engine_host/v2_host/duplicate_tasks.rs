//! Shared deterministic duplicate-task assessment for finding sweep actions.
//!
//! Exact generated-key tags remain the authoritative fast path. When that
//! path misses, rejected exact-key tasks may name one still-open covering
//! owner in a duplicate comment. Otherwise a candidate is compared with
//! every still-open task using caller-supplied, high-confidence fingerprints.
//! A fingerprint must match in full; individual keywords never carry a
//! duplicate decision.

use orbit_common::security::redaction::redact_all;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{Task, TaskComment, TaskStatus, is_valid_orb_task_id};
use serde_json::{Value, json};

const MAX_EVIDENCE_FIELDS: usize = 6;
const MAX_EVIDENCE_CHARS: usize = 160;

const COVERING_OWNER_MARKERS: &[&str] = &[
    "duplicate of",
    "covering implementation",
    "covering task",
    "covering repair",
    "covering work",
    "covered by",
];

pub(in crate::adapter::engine_host::v2_host) trait DuplicateTaskLookup {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError>;

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError>;

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError>;

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError>;
}

impl DuplicateTaskLookup for crate::OrbitRuntime {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        crate::OrbitRuntime::list_tasks_by_tags(self, tags)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        crate::OrbitRuntime::list_tasks(self)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        crate::OrbitRuntime::get_task(self, task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        crate::OrbitRuntime::get_task_comments(self, task_id)
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct DuplicateCandidate {
    exact_tag: String,
    fingerprints: Vec<CoverageFingerprint>,
    completed_fingerprints: Vec<CoverageFingerprint>,
}

impl DuplicateCandidate {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        exact_tag: String,
        fingerprints: Vec<CoverageFingerprint>,
    ) -> Self {
        Self {
            exact_tag,
            fingerprints,
            completed_fingerprints: Vec::new(),
        }
    }

    pub(in crate::adapter::engine_host::v2_host) fn with_completed_fingerprints(
        mut self,
        fingerprints: Vec<CoverageFingerprint>,
    ) -> Self {
        self.completed_fingerprints = fingerprints;
        self
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
/// lookup is neither needed nor allowed to override it. Rejected exact-key
/// tasks are not coverage on their own; they may point at one still-open
/// owner through an explicit duplicate comment.
pub(in crate::adapter::engine_host::v2_host) fn find_covering_task<L>(
    lookup: &L,
    candidate: &DuplicateCandidate,
) -> Result<Option<DuplicateTaskMatch>, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    let exact_tasks = lookup.list_tasks_by_tags(std::slice::from_ref(&candidate.exact_tag))?;
    if let Some(task) = exact_tasks.iter().find(|task| is_open_status(task.status)) {
        return Ok(Some(DuplicateTaskMatch {
            task_id: task.id.clone(),
            match_kind: "exact_key",
            evidence: bounded_evidence(
                "generated_key",
                &[CoverageAnchor::new("dedupe_tag", &candidate.exact_tag)],
            ),
        }));
    }

    if let Some(confirmed) = confirmed_duplicate_match(lookup, candidate, &exact_tasks)? {
        return Ok(Some(confirmed));
    }

    validate_candidate(candidate)?;
    let mut open_tasks = lookup
        .list_tasks()?
        .into_iter()
        .filter(|task| is_open_status(task.status) || recently_completed(task))
        .collect::<Vec<_>>();
    open_tasks.sort_by(|left, right| left.id.cmp(&right.id));

    for task in open_tasks {
        let comments = lookup.get_task_comments(&task.id)?;
        let searchable = searchable_task_text(&task, &comments);
        let fingerprints = if is_open_status(task.status) {
            &candidate.fingerprints
        } else {
            &candidate.completed_fingerprints
        };
        if let Some(fingerprint) = fingerprints
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

fn confirmed_duplicate_match<L>(
    lookup: &L,
    candidate: &DuplicateCandidate,
    exact_tasks: &[Task],
) -> Result<Option<DuplicateTaskMatch>, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    let mut rejected = exact_tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Rejected)
        .collect::<Vec<_>>();
    if rejected.is_empty() {
        return Ok(None);
    }
    rejected.sort_by(|left, right| left.id.cmp(&right.id));

    for task in rejected {
        let comments = lookup.get_task_comments(&task.id)?;
        let Some(owner_id) = covering_owner_from_comments(&task.id, &comments) else {
            continue;
        };
        match lookup.get_task(&owner_id) {
            Ok(owner) if is_open_status(owner.status) => {
                return Ok(Some(DuplicateTaskMatch {
                    task_id: owner.id.clone(),
                    match_kind: "confirmed_duplicate",
                    evidence: bounded_evidence(
                        "rejected_duplicate_comment",
                        &[
                            CoverageAnchor::new("rejected_task_id", task.id.as_str()),
                            CoverageAnchor::new("covering_task_id", owner.id.as_str()),
                            CoverageAnchor::new("dedupe_tag", &candidate.exact_tag),
                        ],
                    ),
                }));
            }
            Ok(_) => continue,
            Err(error) if is_task_not_found(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn covering_owner_from_comments(rejected_id: &str, comments: &[TaskComment]) -> Option<String> {
    let mut owner = None;
    for comment in comments {
        if let Some(found) = covering_owner_from_message(rejected_id, &comment.message) {
            owner = Some(found);
        }
    }
    owner
}

fn covering_owner_from_message(rejected_id: &str, message: &str) -> Option<String> {
    let lowered = message.to_ascii_lowercase();
    let mut owner = None;
    for marker in COVERING_OWNER_MARKERS {
        let mut from = 0;
        while let Some(pos) = lowered[from..].find(marker) {
            let after = from + pos + marker.len();
            if let Some(id) = first_task_id_in(&message[after..])
                && id != rejected_id
            {
                owner = Some(id);
            }
            from = after;
        }
    }
    owner
}

fn first_task_id_in(text: &str) -> Option<String> {
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_alphanumeric() || character == '-' {
            current.push(character);
            continue;
        }
        if let Some(id) = take_task_id(&mut current) {
            return Some(id);
        }
    }
    take_task_id(&mut current)
}

fn take_task_id(current: &mut String) -> Option<String> {
    let candidate = std::mem::take(current);
    is_valid_orb_task_id(&candidate).then_some(candidate)
}

fn is_task_not_found(error: &OrbitError) -> bool {
    matches!(
        error,
        OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        }
    )
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

fn searchable_task_text(task: &Task, comments: &[TaskComment]) -> String {
    let mut text = String::new();
    for value in std::iter::once(task.title.as_str())
        .chain(std::iter::once(task.description.as_str()))
        .chain(task.acceptance_criteria.iter().map(String::as_str))
        .chain(std::iter::once(task.plan.as_str()))
        .chain(task.tags.iter().map(String::as_str))
        .chain(task.context_files.iter().map(String::as_str))
        .chain(task.external_refs.iter().flat_map(|reference| {
            [
                reference.system.as_str(),
                reference.id.as_str(),
                reference.url.as_deref().unwrap_or(""),
            ]
        }))
        .chain(comments.iter().map(|comment| comment.message.as_str()))
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

fn recently_completed(task: &Task) -> bool {
    task.status == TaskStatus::Done
        && task.updated_at >= chrono::Utc::now() - chrono::Duration::days(30)
}

#[cfg(test)]
mod tests {
    use super::{canonical_text, covering_owner_from_message};

    #[test]
    fn canonical_text_preserves_token_boundaries() {
        let searchable = canonical_text("Update runtime in Cargo.lock");
        assert!(!searchable.contains(&canonical_text("time")));
        assert!(searchable.contains(&canonical_text("runtime")));
    }

    #[test]
    fn covering_owner_parses_duplicate_and_covering_phrases() {
        assert_eq!(
            covering_owner_from_message(
                "ORB-11513",
                "Duplicate of active ORB-11511: both cite Wrangler 4.129.0.",
            )
            .as_deref(),
            Some("ORB-11511")
        );
        assert_eq!(
            covering_owner_from_message(
                "ORB-11526",
                "Covering implementation is active ORB-11511 (same Wrangler missing-name error).",
            )
            .as_deref(),
            Some("ORB-11511")
        );
        assert_eq!(
            covering_owner_from_message("ORB-11513", "Won't fix; infrastructure flake."),
            None
        );
        assert_eq!(
            covering_owner_from_message("ORB-11513", "Duplicate of active ORB-11513 itself."),
            None
        );
    }
}
