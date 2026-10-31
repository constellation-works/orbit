//! Explore briefs: the pilot's bounded, revision-keyed map of a task's change.
//!
//! A brief is an optional part of a `selectors` assessment for a task whose
//! preparation requested one (`workflow.explore_brief.*`). Apply validates it
//! here and the persist step writes it as the system-written
//! [`EXPLORE_BRIEF_ARTIFACT`] before the receipt. A brief never fails its
//! assessment: an invalid one is dropped, and the reason is recorded in the
//! receipt's audit note instead.

use std::path::Path;
use std::str::FromStr;

use orbit_common::fs::selector::Selector;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{EXPLORE_BRIEF_ARTIFACT, TaskArtifact};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::source::{GitPathKind, SourceSnapshot};

/// Assessment field carrying the pilot's brief. Apply removes it before the
/// assessment is fingerprinted or audited: only the validated artifact keeps it.
pub(super) const EXPLORE_BRIEF_FIELD: &str = "explore_brief";
pub(super) const EXPLORE_BRIEF_SCHEMA: &str = "explore_brief.v1";
/// The whole stored document, provenance included: about 2k tokens, the same
/// limit a tag instruction has, so a brief stays a map and not a transcript.
const MAX_DOCUMENT_BYTES: usize = 8 * 1024;
const MAX_CHANGE: usize = 12;
const MAX_CALL_PATH: usize = 10;
const MAX_PATTERN: usize = 3;
const MAX_TESTS: usize = 6;
const MAX_GOTCHAS: usize = 6;
const MAX_OUT_OF_SCOPE: usize = 5;
const MAX_MAPS: usize = 4;
const MAX_WHAT_CHARS: usize = 200;
const MAX_GOTCHA_CHARS: usize = 300;
const MAX_OUT_OF_SCOPE_CHARS: usize = 200;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Brief {
    change: Vec<Change>,
    #[serde(default)]
    call_path: Vec<CallStep>,
    #[serde(default)]
    pattern: Vec<Pattern>,
    #[serde(default)]
    tests: Vec<Covering>,
    #[serde(default)]
    gotchas: Vec<Gotcha>,
    #[serde(default)]
    out_of_scope: Vec<String>,
    #[serde(default)]
    maps: Vec<String>,
    confidence: Confidence,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Change {
    target: String,
    what: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CallStep {
    target: String,
    role: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pattern {
    target: String,
    why: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Covering {
    target: String,
    covers: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Gotcha {
    text: String,
    cite: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Confidence {
    High,
    Medium,
    Low,
}

/// What apply decided about one task's brief.
pub(in super::super) enum BriefDecision {
    /// Validated against the pinned revision; written before the receipt.
    Accepted(ValidBrief),
    /// Not written; the reason goes into the receipt's audit note.
    Dropped(String),
}

/// A brief that passed every rule, with the inspection proof it was read at.
pub(in super::super) struct ValidBrief {
    brief: Value,
    inspection_revision: String,
    observed_tree: Option<String>,
}

/// What the assessed task offers a brief to be checked against.
pub(super) struct BriefScope<'a> {
    pub(super) requested: bool,
    pub(super) disposition: &'a str,
    pub(super) recommended_complexity: Option<&'a str>,
    /// The final lock footprint every `change` target must lie inside.
    pub(super) after: &'a [String],
    /// Creation targets the task's durable grant names; the only targets
    /// that may be missing at the pinned revision.
    pub(super) authorized_creation: &'a [String],
    pub(super) workspace_root: &'a Path,
    pub(super) source: Option<&'a SourceSnapshot>,
    pub(super) source_tree: Option<&'a str>,
}

/// Remove the pilot's brief from `assessment`, so it never reaches the
/// receipt's fingerprint or audit. `null` reads as no brief.
pub(super) fn take_explore_brief(assessment: &mut Value) -> Option<Value> {
    assessment
        .as_object_mut()
        .and_then(|fields| fields.remove(EXPLORE_BRIEF_FIELD))
        .filter(|brief| !brief.is_null())
}

/// Decide a task's brief. `None` when nothing is owed and nothing returned:
/// no brief was requested, or the assessment is one that gets no brief.
pub(super) fn decide_explore_brief(
    action: &str,
    brief: Option<Value>,
    scope: &BriefScope<'_>,
) -> Option<BriefDecision> {
    let ineligible = if scope.disposition != "selectors" {
        Some(format!("no brief for a {} assessment", scope.disposition))
    } else if scope.recommended_complexity == Some("unassessed") {
        Some("no brief for an unassessed task".to_string())
    } else {
        None
    };
    let Some(brief) = brief else {
        return (scope.requested && ineligible.is_none() && scope.source.is_some())
            .then(|| BriefDecision::Dropped("requested but not returned".to_string()));
    };
    if !scope.requested {
        return Some(BriefDecision::Dropped(
            "returned for a task that did not request one".to_string(),
        ));
    }
    if let Some(reason) = ineligible {
        return Some(BriefDecision::Dropped(reason));
    }
    let Some(source) = scope.source else {
        return Some(BriefDecision::Dropped(
            "a non-Git workspace has no revision to key a brief to".to_string(),
        ));
    };
    Some(match validate(action, brief, scope, source) {
        Ok(brief) => BriefDecision::Accepted(ValidBrief {
            brief,
            inspection_revision: source.source_revision.clone(),
            observed_tree: scope.source_tree.map(str::to_string),
        }),
        Err(reason) => BriefDecision::Dropped(reason),
    })
}

fn validate(
    action: &str,
    brief: Value,
    scope: &BriefScope<'_>,
    source: &SourceSnapshot,
) -> Result<Value, String> {
    let mut brief: Brief =
        serde_json::from_value(brief).map_err(|error| format!("schema: {error}"))?;
    if brief.change.is_empty() {
        return Err("schema: `change` must name at least one target".to_string());
    }
    for (field, count, cap) in [
        ("change", brief.change.len(), MAX_CHANGE),
        ("call_path", brief.call_path.len(), MAX_CALL_PATH),
        ("pattern", brief.pattern.len(), MAX_PATTERN),
        ("tests", brief.tests.len(), MAX_TESTS),
        ("gotchas", brief.gotchas.len(), MAX_GOTCHAS),
        ("out_of_scope", brief.out_of_scope.len(), MAX_OUT_OF_SCOPE),
        ("maps", brief.maps.len(), MAX_MAPS),
    ] {
        if count > cap {
            return Err(format!("`{field}` has {count} entries; the cap is {cap}"));
        }
    }
    let mut text = Vec::new();
    for change in &brief.change {
        text.push(("change.what", change.what.as_str(), Some(MAX_WHAT_CHARS)));
    }
    for step in &brief.call_path {
        text.push(("call_path.role", step.role.as_str(), None));
    }
    for pattern in &brief.pattern {
        text.push(("pattern.why", pattern.why.as_str(), None));
    }
    for covering in &brief.tests {
        text.push(("tests.covers", covering.covers.as_str(), None));
    }
    for gotcha in &brief.gotchas {
        text.push(("gotchas.text", gotcha.text.as_str(), Some(MAX_GOTCHA_CHARS)));
    }
    for item in &brief.out_of_scope {
        text.push(("out_of_scope", item.as_str(), Some(MAX_OUT_OF_SCOPE_CHARS)));
    }
    for (field, value, cap) in text {
        check_text(field, value, cap)?;
    }

    let anchors = Anchors {
        action,
        scope,
        source,
    };
    for change in &mut brief.change {
        change.target = anchors.check("change.target", &change.target)?;
        if !inside_footprint(&change.target, scope.after) {
            return Err(format!(
                "change target {} lies outside context_files_after",
                change.target
            ));
        }
    }
    for step in &mut brief.call_path {
        step.target = anchors.check("call_path.target", &step.target)?;
    }
    for pattern in &mut brief.pattern {
        pattern.target = anchors.check("pattern.target", &pattern.target)?;
    }
    for covering in &mut brief.tests {
        covering.target = anchors.check("tests.target", &covering.target)?;
    }
    for gotcha in &mut brief.gotchas {
        gotcha.cite = anchors.check("gotchas.cite", &gotcha.cite)?;
    }
    for map in &mut brief.maps {
        *map = anchors.check("maps", map)?;
        if !map.starts_with("file:") {
            return Err(format!("map {map} must be a file: selector"));
        }
    }
    serde_json::to_value(&brief).map_err(|error| format!("serialize brief: {error}"))
}

/// Free text describes; it never pastes code and never cites a line.
fn check_text(field: &str, value: &str, cap: Option<usize>) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("`{field}` is empty"));
    }
    if let Some(cap) = cap
        && value.chars().count() > cap
    {
        return Err(format!("`{field}` is longer than {cap} characters"));
    }
    if value.contains("```") || value.contains(['\n', '\r']) {
        return Err(format!(
            "`{field}` pastes code: free text is one line with no fenced block"
        ));
    }
    if let Some(reference) = line_reference(value) {
        return Err(format!("`{field}` cites a line number ({reference})"));
    }
    Ok(())
}

/// The first line-number reference in `text`: `path:<digits>`, `path#L<digits>`
/// or `line <digits>`. Line numbers break with the next merge.
fn line_reference(text: &str) -> Option<String> {
    let words = text
        .split(|ch: char| ch.is_whitespace() || "`'\"()[]<>,;".contains(ch))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    for (index, word) in words.iter().enumerate() {
        let lower = word.to_ascii_lowercase();
        if matches!(lower.trim_end_matches('.'), "line" | "lines" | "l.")
            && words
                .get(index + 1)
                .is_some_and(|next| next.starts_with(|ch: char| ch.is_ascii_digit()))
        {
            return Some(format!("{word} {}", words[index + 1]));
        }
        if let Some((_, after)) = word.split_once("#L")
            && after.starts_with(|ch: char| ch.is_ascii_digit())
        {
            return Some((*word).to_string());
        }
        let mut search = word.char_indices().filter(|(_, ch)| *ch == ':');
        if search.any(|(at, _)| {
            let (head, tail) = word.split_at(at);
            tail[1..].starts_with(|ch: char| ch.is_ascii_digit()) && head.contains(['.', '/'])
        }) {
            return Some((*word).to_string());
        }
    }
    None
}

/// Every brief target is a selector whose anchor exists at the pinned
/// revision, the same rule `context_files_after` follows.
struct Anchors<'a> {
    action: &'a str,
    scope: &'a BriefScope<'a>,
    source: &'a SourceSnapshot,
}

impl Anchors<'_> {
    /// The canonical form of `target`, or why it is refused.
    fn check(&self, field: &str, target: &str) -> Result<String, String> {
        if let Some(reference) = line_reference(target) {
            return Err(format!(
                "`{field}` {target} cites a line number ({reference})"
            ));
        }
        let selector = Selector::from_str(target)
            .map_err(|error| format!("`{field}` {target} is not a selector: {}", error.reason))?;
        let canonical = selector.to_string();
        let (path, symbol, want_dir) = match &selector {
            Selector::File { path } => (path, None, false),
            Selector::Dir { path } => (path, None, true),
            Selector::Symbol { path, symbol, .. } => (path, Some(symbol), false),
            Selector::Module { .. } | Selector::Command { .. } => {
                return Err(format!(
                    "`{field}` {target} must be a file:, dir: or symbol: selector"
                ));
            }
        };
        if self
            .scope
            .authorized_creation
            .iter()
            .any(|granted| granted == &canonical)
        {
            return Ok(canonical);
        }
        let kind = self
            .source
            .path_kind(self.action, self.scope.workspace_root, Path::new(path))
            .map_err(|error| format!("`{field}` {target}: {error}"))?;
        match (kind, want_dir) {
            (GitPathKind::Tree, true) | (GitPathKind::Blob, false) => {}
            (GitPathKind::Missing, _) => {
                return Err(format!(
                    "`{field}` {target} does not exist at the inspection revision"
                ));
            }
            _ => {
                return Err(format!(
                    "`{field}` {target} does not match the target's file/directory kind"
                ));
            }
        }
        if let Some(symbol) = symbol {
            let name = symbol
                .rsplit("::")
                .next()
                .and_then(|name| name.rsplit('.').next())
                .unwrap_or(symbol);
            let text = self
                .source
                .file_text(self.action, self.scope.workspace_root, path)
                .map_err(|error| format!("`{field}` {target}: {error}"))?;
            if !text.is_some_and(|text| text.contains(name)) {
                return Err(format!(
                    "`{field}` {target}: {name} does not occur in {path} at the inspection revision"
                ));
            }
        }
        Ok(canonical)
    }
}

/// Whether the lock footprint covers `target`: exactly, through a `dir:`
/// containing it, or through the `file:` a `symbol:` target lives in.
fn inside_footprint(target: &str, after: &[String]) -> bool {
    let Ok(target) = Selector::from_str(target) else {
        return false;
    };
    let Some(path) = target.anchor_path() else {
        return false;
    };
    after
        .iter()
        .any(|selector| match Selector::from_str(selector) {
            Ok(Selector::Dir { path: dir }) => {
                dir == "."
                    || path == dir
                    || path.starts_with(&format!("{}/", dir.trim_end_matches('/')))
            }
            Ok(Selector::File { path: file }) => {
                file == path && !matches!(target, Selector::Dir { .. })
            }
            Ok(selector @ Selector::Symbol { .. }) => selector == target,
            _ => false,
        })
}

/// The provenance apply adds to a validated brief once the receipt that
/// carries it is known.
pub(in super::super) struct BriefProvenance<'a> {
    pub(in super::super) task_id: &'a str,
    pub(in super::super) material_fingerprint: Option<&'a str>,
    pub(in super::super) receipt_operation_id: &'a str,
    pub(in super::super) pilot_run_id: Option<&'a str>,
    pub(in super::super) pilot_crew: Option<&'a str>,
}

/// Write an accepted brief through the system writer, ahead of the receipt
/// it names, and return its audit record. A brief that cannot be stored is
/// dropped with the reason; it never fails the assessment.
pub(in super::super) fn store_explore_brief(
    runtime: &OrbitRuntime,
    decision: &BriefDecision,
    provenance: &BriefProvenance<'_>,
) -> Value {
    let brief = match decision {
        BriefDecision::Accepted(brief) => brief,
        BriefDecision::Dropped(reason) => return dropped(reason),
    };
    let Some(material_fingerprint) = provenance.material_fingerprint else {
        return dropped("the assessment certified no material fingerprint to key a brief to");
    };
    let document = json!({
        "schema": EXPLORE_BRIEF_SCHEMA,
        "task_id": provenance.task_id,
        "inspection_revision": brief.inspection_revision,
        "observed_tree": brief.observed_tree,
        "material_fingerprint": material_fingerprint,
        "receipt_operation_id": provenance.receipt_operation_id,
        "pilot_run_id": provenance.pilot_run_id,
        "pilot_crew": provenance.pilot_crew,
        "brief": brief.brief,
    });
    let content = match serde_json::to_vec(&document) {
        Ok(content) => content,
        Err(error) => return dropped(&format!("serialize brief: {error}")),
    };
    if content.len() > MAX_DOCUMENT_BYTES {
        return dropped(&format!(
            "the brief is {} bytes; the cap is {MAX_DOCUMENT_BYTES}",
            content.len()
        ));
    }
    let sha256 = sha256_hex(&content);
    let bytes = content.len();
    let written = runtime.update_task_as_system(
        provenance.task_id,
        TaskUpdateParams {
            upsert_artifacts: vec![TaskArtifact {
                path: EXPLORE_BRIEF_ARTIFACT.to_string(),
                content,
                media_type: "application/json".to_string(),
                created_by: None,
            }],
            ..TaskUpdateParams::default()
        },
        None,
    );
    match written {
        Ok(_) => json!({
            "written": EXPLORE_BRIEF_ARTIFACT,
            "sha256": sha256,
            "bytes": bytes,
        }),
        Err(error) => dropped(&format!("write {EXPLORE_BRIEF_ARTIFACT}: {error}")),
    }
}

fn dropped(reason: &str) -> Value {
    json!({ "dropped": reason })
}
