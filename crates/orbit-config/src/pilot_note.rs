//! `[workflow.pilot_notes.<label>]`: repository files that tell the task pilot
//! what per-task note to write under `<label>`.
//!
//! Each table maps `instruction` to a path relative to the repository root, or
//! to `false` in a workspace layer to switch off a note the global config
//! defines. Tables layer like `[workflow.tag_instructions]`: a workspace table
//! replaces the global one of the same label. At most four active notes may be
//! configured per workspace.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::{MAX_PILOT_NOTES, PILOT_NOTES_KEY, validate_pilot_note_label};

use crate::config_document;
use crate::tag_instructions::repository_path;

/// The admitted pilot-notes table, keyed by label in sorted order.
/// Labels switched off with `instruction = false` are omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PilotNoteSettings {
    notes: BTreeMap<String, String>,
}

impl PilotNoteSettings {
    /// Every enabled `(label, path)` mapping, in sorted label order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.notes
            .iter()
            .map(|(label, path)| (label.as_str(), path.as_str()))
    }

    /// Number of enabled pilot notes.
    pub fn len(&self) -> usize {
        self.notes.len()
    }

    /// Whether no pilot note is enabled.
    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }
}

/// Admit `workflow.pilot_notes` from one (possibly merged) document.
pub(crate) fn pilot_notes_from_document(
    document: &toml::Value,
    config_path: &Path,
) -> Result<PilotNoteSettings, OrbitError> {
    let Some(value) = document
        .get("workflow")
        .and_then(|workflow| workflow.get("pilot_notes"))
    else {
        return Ok(PilotNoteSettings::default());
    };
    let invalid = |detail: String| config_document::invalid_runtime_config(config_path, detail);
    let Some(table) = value.as_table() else {
        return Err(invalid(format!(
            "'{PILOT_NOTES_KEY}' must be a table of `[{PILOT_NOTES_KEY}.<label>]` tables"
        )));
    };
    let mut notes = BTreeMap::new();
    for (label, entry) in table {
        let key = format!("{PILOT_NOTES_KEY}.{label}");
        validate_pilot_note_label(label).map_err(|why| invalid(format!("'{key}': {why}")))?;
        let Some(entry_table) = entry.as_table() else {
            return Err(invalid(format!(
                "'{key}' must be a table with `instruction = \"<path>\"` or `instruction = false`"
            )));
        };
        for field in entry_table.keys() {
            if field != "instruction" {
                return Err(invalid(format!(
                    "'{key}.{field}' is not a pilot-note setting; use `instruction`"
                )));
            }
        }
        let Some(instruction) = entry_table.get("instruction") else {
            return Err(invalid(format!(
                "'{key}.instruction' is required: set a repository-relative file path or false"
            )));
        };
        match instruction {
            toml::Value::Boolean(false) => {}
            toml::Value::String(raw) => {
                let path = repository_path(raw)
                    .map_err(|why| invalid(format!("'{key}.instruction' {why}")))?;
                notes.insert(label.clone(), path);
            }
            _ => {
                return Err(invalid(format!(
                    "'{key}.instruction' must be a repository-relative file path or false"
                )));
            }
        }
    }
    if notes.len() > MAX_PILOT_NOTES {
        let labels = notes.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(invalid(format!(
            "'{PILOT_NOTES_KEY}' maps {} notes ({labels}); at most {MAX_PILOT_NOTES} are allowed",
            notes.len()
        )));
    }
    Ok(PilotNoteSettings { notes })
}
