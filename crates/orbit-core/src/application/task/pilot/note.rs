//! Pilot notes in an assessment: the checks apply makes on each returned note.
//! The readout of the notes a task's current assessment holds is
//! `OrbitRuntime::pilot_notes_readout`.
//!
//! A note is the pilot's short text for one task under a label the workspace
//! mapped (`[workflow.pilot_notes.<label>]`). It lives in the assessment
//! artifact apply already stores, beside everything else the pilot said, so
//! the latest applied assessment is the whole truth about a task's notes.
//! Orbit never reads a note's meaning: admission, attention, the drain and
//! freshness look at none of it, and no prompt Orbit builds carries it.

use std::collections::BTreeSet;

use orbit_types::workflow::{
    MAX_PILOT_NOTE_LABEL_LEN, MAX_PILOT_NOTE_TEXT_BYTES, MAX_PILOT_NOTES, PilotNoteInstruction,
};
use serde_json::{Value, json};

/// Assessment field holding the kept notes: `{label, text, instruction}`,
/// where `instruction` names the path, revision and SHA-256 the run was given.
pub(crate) const NOTES: &str = "notes";

/// Assessment field listing each dropped note's label and reason.
pub(crate) const NOTES_DROPPED: &str = "notes_dropped";

/// Check the notes the pilot returned in `assessment`, whatever they hold,
/// and keep the rest of the assessment untouched. `given` is every note
/// instruction the run's prompt carried. A note is dropped, never repaired
/// and never an error, for one reason: `too_many` (the fifth or later of the
/// entry), `unknown_label`, `duplicate_label` (a repeat of a label already
/// seen; the first occurrence stands), `empty`, `too_large` or
/// `not_plain_text`.
///
/// Both fields are host-owned: a pilot-supplied `notes_dropped` never
/// survives, and an assessment whose pilot returned no note carries neither
/// field.
pub(super) fn settle_notes(assessment: &mut Value, given: &[PilotNoteInstruction]) {
    let Value::Object(fields) = assessment else {
        return;
    };
    fields.remove(NOTES_DROPPED);
    let Some(returned) = fields.remove(NOTES) else {
        return;
    };
    let Value::Array(returned) = returned else {
        return;
    };
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut seen = BTreeSet::new();
    for (position, note) in returned.iter().enumerate() {
        let label = note
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match refusal(position, note, label, given, &mut seen) {
            Ok((text, instruction)) => kept.push(json!({
                "label": label,
                "text": text,
                "instruction": {
                    "path": instruction.path,
                    "revision": instruction.revision,
                    "sha256": instruction.sha256,
                },
            })),
            Err(reason) => dropped.push(json!({
                "label": label.chars().take(MAX_PILOT_NOTE_LABEL_LEN).collect::<String>(),
                "reason": reason,
            })),
        }
    }
    if !kept.is_empty() {
        fields.insert(NOTES.to_string(), Value::Array(kept));
    }
    if !dropped.is_empty() {
        fields.insert(NOTES_DROPPED.to_string(), Value::Array(dropped));
    }
}

/// The note's text and instruction when it is kept, or why it is dropped.
fn refusal<'a>(
    position: usize,
    note: &'a Value,
    label: &'a str,
    given: &'a [PilotNoteInstruction],
    seen: &mut BTreeSet<&'a str>,
) -> Result<(&'a str, &'a PilotNoteInstruction), &'static str> {
    if position >= MAX_PILOT_NOTES {
        return Err("too_many");
    }
    let instruction = given
        .iter()
        .find(|instruction| instruction.label == label)
        .ok_or("unknown_label")?;
    if !seen.insert(label) {
        return Err("duplicate_label");
    }
    let text = note.get("text").and_then(Value::as_str).unwrap_or_default();
    if text.trim().is_empty() {
        return Err("empty");
    }
    if text.len() > MAX_PILOT_NOTE_TEXT_BYTES {
        return Err("too_large");
    }
    if text
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err("not_plain_text");
    }
    Ok((text, instruction))
}
