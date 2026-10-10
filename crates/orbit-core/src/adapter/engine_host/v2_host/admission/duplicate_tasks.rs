//! Shared deterministic duplicate-task assessment for finding sweep actions.
//!
//! Exact generated-key tags remain the authoritative fast path. When that
//! path misses, rejected exact-key tasks may name one still-open covering
//! owner in a duplicate comment. Otherwise a candidate is compared with
//! every still-open task using caller-supplied, high-confidence fingerprints.
//! A fingerprint must match in full; individual keywords never carry a
//! duplicate decision.
//!
//! A candidate may also carry a rejected-owner fingerprint. An exact-key task
//! that was rejected on its own merits then suppresses the candidate for as
//! long as that fingerprint still matches the rejected task's text, so a
//! finding an implementer correctly refused is not re-filed unchanged.
//! Anchors the fingerprint marks as colocated must come from one bullet of a
//! per-alert evidence ledger when the rejected description has one, so a
//! grouped owner cannot satisfy a changed alert with a sibling's location.

use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use orbit_common::security::redaction::redact_all;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{Task, TaskComment, TaskStatus, is_valid_orb_task_id};
use serde_json::{Value, json};

/// `match_kind` of a match that is a rejected owner, not an open one.
pub(in crate::adapter::engine_host::v2_host) const MATCH_REJECTED_OWNER: &str = "rejected_owner";

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

    /// Shared so a snapshot hands the same allocation to every candidate
    /// instead of cloning the workspace's task list per assessment.
    fn list_tasks(&self) -> Result<Rc<[Task]>, OrbitError>;

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError>;

    fn get_task_comments(&self, task_id: &str) -> Result<Rc<[TaskComment]>, OrbitError>;
}

impl DuplicateTaskLookup for crate::OrbitRuntime {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        crate::OrbitRuntime::list_tasks_by_tags(self, tags)
    }

    fn list_tasks(&self) -> Result<Rc<[Task]>, OrbitError> {
        crate::OrbitRuntime::list_tasks(self).map(Rc::from)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        crate::OrbitRuntime::get_task(self, task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Rc<[TaskComment]>, OrbitError> {
        crate::OrbitRuntime::get_task_comments(self, task_id).map(Rc::from)
    }
}

/// One action's read-through snapshot over another lookup.
///
/// `find_covering_task` lists every task and reads every open task's comments
/// once per candidate. An action that assesses many candidates — one per
/// cluster and one per legacy key in the CI sweep — would otherwise
/// re-hydrate the whole workspace for each of them. The snapshot fills
/// lazily, so the exact-key fast path still never lists broadly, and it is
/// dropped with the action, so nothing it caches can outlive the reads it
/// was taken from. Tag queries and direct task reads pass through: they are
/// indexed or single-bundle reads and happen only on a hit.
pub(in crate::adapter::engine_host::v2_host) struct SnapshotDuplicateLookup<'a, L: ?Sized> {
    inner: &'a L,
    tasks: OnceCell<Rc<[Task]>>,
    comments: RefCell<BTreeMap<String, Rc<[TaskComment]>>>,
}

impl<'a, L: DuplicateTaskLookup + ?Sized> SnapshotDuplicateLookup<'a, L> {
    pub(in crate::adapter::engine_host::v2_host) fn new(inner: &'a L) -> Self {
        Self {
            inner,
            tasks: OnceCell::new(),
            comments: RefCell::new(BTreeMap::new()),
        }
    }
}

impl<L: DuplicateTaskLookup + ?Sized> DuplicateTaskLookup for SnapshotDuplicateLookup<'_, L> {
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.inner.list_tasks_by_tags(tags)
    }

    fn list_tasks(&self) -> Result<Rc<[Task]>, OrbitError> {
        if let Some(tasks) = self.tasks.get() {
            return Ok(Rc::clone(tasks));
        }
        // A failed hydration is not cached: the caller fails the whole action
        // on the first error, so there is no retry to serve stale emptiness to.
        let tasks = self.inner.list_tasks()?;
        Ok(Rc::clone(self.tasks.get_or_init(|| tasks)))
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.inner.get_task(task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Rc<[TaskComment]>, OrbitError> {
        if let Some(comments) = self.comments.borrow().get(task_id) {
            return Ok(Rc::clone(comments));
        }
        let comments = self.inner.get_task_comments(task_id)?;
        self.comments
            .borrow_mut()
            .insert(task_id.to_string(), Rc::clone(&comments));
        Ok(comments)
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct DuplicateCandidate {
    exact_tag: String,
    fingerprints: Vec<CoverageFingerprint>,
    completed_fingerprints: Vec<CoverageFingerprint>,
    rejected_owner_fingerprint: Option<CoverageFingerprint>,
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
            rejected_owner_fingerprint: None,
        }
    }

    pub(in crate::adapter::engine_host::v2_host) fn with_completed_fingerprints(
        mut self,
        fingerprints: Vec<CoverageFingerprint>,
    ) -> Self {
        self.completed_fingerprints = fingerprints;
        self
    }

    /// Suppress the candidate while a task rejected under its exact key still
    /// records everything `fingerprint` names. A rejection that points at a
    /// covering owner is a duplicate verdict, not one on the finding, and never
    /// suppresses.
    pub(in crate::adapter::engine_host::v2_host) fn with_rejected_owner_fingerprint(
        mut self,
        fingerprint: CoverageFingerprint,
    ) -> Self {
        self.rejected_owner_fingerprint = Some(fingerprint);
        self
    }
}

#[derive(Debug, Clone)]
pub(in crate::adapter::engine_host::v2_host) struct CoverageFingerprint {
    name: &'static str,
    anchors: Vec<CoverageAnchor>,
    /// Anchor `field` names that a rejected-owner match must satisfy together
    /// from one per-alert ledger bullet. Empty keeps whole-text matching.
    colocated_fields: &'static [&'static str],
}

impl CoverageFingerprint {
    pub(in crate::adapter::engine_host::v2_host) fn new(
        name: &'static str,
        anchors: Vec<CoverageAnchor>,
    ) -> Self {
        Self {
            name,
            anchors,
            colocated_fields: &[],
        }
    }

    /// Require `fields` to co-occur in one ledger bullet when the rejected
    /// task records a per-alert ledger. A body without that ledger, such as
    /// a single-alert record, still matches these anchors across the whole
    /// task text.
    pub(in crate::adapter::engine_host::v2_host) fn with_colocated_fields(
        mut self,
        fields: &'static [&'static str],
    ) -> Self {
        self.colocated_fields = fields;
        self
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
/// owner through an explicit duplicate comment, or suppress the candidate when
/// it opted into a rejected-owner fingerprint that still matches.
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
    let all_tasks = lookup.list_tasks()?;
    let mut open_tasks = all_tasks
        .iter()
        .filter(|task| is_open_status(task.status) || recently_completed(task))
        .collect::<Vec<_>>();
    open_tasks.sort_by(|left, right| left.id.cmp(&right.id));

    for task in open_tasks {
        let comments = lookup.get_task_comments(&task.id)?;
        let searchable = searchable_task_text(task, &comments);
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
                task_id: task.id.clone(),
                match_kind: "material_coverage",
                evidence: bounded_evidence(fingerprint.name, &fingerprint.anchors),
            }));
        }
    }

    rejected_owner_match(lookup, candidate, &exact_tasks)
}

fn rejected_owner_match<L>(
    lookup: &L,
    candidate: &DuplicateCandidate,
    exact_tasks: &[Task],
) -> Result<Option<DuplicateTaskMatch>, OrbitError>
where
    L: DuplicateTaskLookup + ?Sized,
{
    let Some(fingerprint) = &candidate.rejected_owner_fingerprint else {
        return Ok(None);
    };
    let mut rejected = exact_tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Rejected)
        .collect::<Vec<_>>();
    rejected.sort_by(|left, right| left.id.cmp(&right.id));

    for task in rejected {
        let comments = lookup.get_task_comments(&task.id)?;
        if covering_owner_from_comments(&task.id, &comments).is_some() {
            continue;
        }
        if !rejected_owner_fingerprint_matches(task, &comments, fingerprint) {
            continue;
        }
        let mut anchors = vec![CoverageAnchor::new("rejected_task_id", task.id.as_str())];
        anchors.extend(fingerprint.anchors.iter().cloned());
        return Ok(Some(DuplicateTaskMatch {
            task_id: task.id.clone(),
            match_kind: MATCH_REJECTED_OWNER,
            evidence: bounded_evidence(fingerprint.name, &anchors),
        }));
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

// Private to this module; exercised through the crate-root runtime tests.
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
    anchors_match(searchable, fingerprint.anchors.iter())
}

/// Persisted heading written by `append_alert_ledger`. Rejected grouped tasks
/// already store these bytes; matching a renamed heading would stop binding
/// an alert to its own location and restore cross-alert suppression.
const PER_ALERT_LEDGER_HEADING: &str = "Per-alert evidence ledger";

fn rejected_owner_fingerprint_matches(
    task: &Task,
    comments: &[TaskComment],
    fingerprint: &CoverageFingerprint,
) -> bool {
    // Exact tokens, not `canonical_text`: a rule id or path that differs only
    // by `-`, `_`, `.` or case is a different finding, and the rejected-owner
    // contract files it again.
    let searchable = exact_tokens(&searchable_task_raw_text(task, comments));
    if fingerprint.colocated_fields.is_empty() {
        return exact_anchors_match(&searchable, fingerprint.anchors.iter());
    }
    // A binding that names an anchor this fingerprint does not carry cannot
    // be checked, so it must not suppress.
    if fingerprint.colocated_fields.iter().any(|field| {
        !fingerprint
            .anchors
            .iter()
            .any(|anchor| anchor.field == *field)
    }) {
        return false;
    }
    let (colocated, rest): (Vec<&CoverageAnchor>, Vec<&CoverageAnchor>) = fingerprint
        .anchors
        .iter()
        .partition(|anchor| fingerprint.colocated_fields.contains(&anchor.field));
    if !exact_anchors_match(&searchable, rest.iter().copied()) {
        return false;
    }
    match per_alert_ledger_bullets(&task.description) {
        Some(bullets) => bullets
            .iter()
            .any(|bullet| exact_anchors_match(&exact_tokens(bullet), colocated.iter().copied())),
        None => exact_anchors_match(&searchable, colocated.iter().copied()),
    }
}

/// Words the sweep writer emits as fixed labels. Their case is prose, so they
/// compare case-insensitively; every other token keeps its exact case.
const EXACT_LABEL_WORDS: [&str; 5] = ["alert", "rule", "location", "line", "lines"];

/// Split text on whitespace and Markdown framing (backticks, `:`, `#`, `*`,
/// commas, semicolons, brackets, pipes) only. Unlike `canonical_text`,
/// `-`, `_`, `.`, `/` and case stay part of a token, so `foo-bar` and
/// `foo_bar` are different tokens.
fn exact_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '`' | ':' | '#' | '*' | ',' | ';' | '(' | ')' | '[' | ']' | '|'
                )
        })
        .filter(|token| !token.is_empty())
        .map(|token| {
            if EXACT_LABEL_WORDS
                .iter()
                .any(|label| token.eq_ignore_ascii_case(label))
            {
                token.to_ascii_lowercase()
            } else {
                token.to_string()
            }
        })
        .collect()
}

/// Every anchor must appear as a contiguous run of whole exact tokens, so
/// `line 12` cannot match `line 120` nor a rule match its `-v2` sibling.
fn exact_anchors_match<'a>(
    tokens: &[String],
    anchors: impl IntoIterator<Item = &'a CoverageAnchor>,
) -> bool {
    anchors.into_iter().all(|anchor| {
        let needle = exact_tokens(&anchor.value);
        !needle.is_empty() && tokens.windows(needle.len()).any(|window| window == needle)
    })
}

/// Bullets under the per-alert ledger, or `None` when the description has no
/// such section. `Some` of an empty list means the heading was present and
/// nothing may satisfy a colocated binding.
fn per_alert_ledger_bullets(description: &str) -> Option<Vec<&str>> {
    let mut bullets = None;
    for line in description.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            if bullets.is_some() {
                break;
            }
            if heading.trim() == PER_ALERT_LEDGER_HEADING {
                bullets = Some(Vec::new());
            }
            continue;
        }
        if let Some(found) = bullets.as_mut()
            && let Some(bullet) = trimmed.strip_prefix("- ")
            && !bullet.is_empty()
        {
            found.push(trimmed);
        }
    }
    bullets
}

fn anchors_match<'a>(
    searchable: &str,
    anchors: impl IntoIterator<Item = &'a CoverageAnchor>,
) -> bool {
    anchors.into_iter().all(|anchor| {
        let needle = canonical_text(&anchor.value);
        searchable.contains(&needle)
    })
}

fn searchable_task_text(task: &Task, comments: &[TaskComment]) -> String {
    canonical_text(&searchable_task_raw_text(task, comments))
}

fn searchable_task_raw_text(task: &Task, comments: &[TaskComment]) -> String {
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
    text
}

/// Lowercase text into a token sequence with a leading and trailing space.
/// Searching canonical anchors in canonical task text therefore preserves
/// token boundaries (`time` cannot match `runtime`) while tolerating normal
/// prose and Markdown punctuation differences.
// Private to this module; exercised through the crate-root runtime tests.
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
