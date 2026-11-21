//! Crew memory's reads and the one write path every writer shares.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{SubsecRound, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{CrewMemoryEventRow, CrewMemoryRevisionRow, CrewMemoryStoreBackend};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    GLOBAL_MAX_BYTES, GLOBAL_MAX_FILES, MODEL_SPACE_MAX_BYTES, MODEL_SPACE_MAX_FILES, MemoryFile,
    MemoryKey, MemoryLintVocabulary, MemoryWrite, MemoryWriteContext, MemoryWriter,
    parse_memory_key, validate_memory_write,
};
use crate::OrbitRuntime;

/// What a dreaming run's write is held to beyond the five rules: the task
/// it writes for, recorded on the revision's event, and the evidence pack
/// its citations must come from.
pub(super) struct DreamingWrite<'a> {
    pub(super) task_id: &'a str,
    pub(super) evidence_pack: &'a BTreeSet<String>,
}

/// The verb and audit action every revision write records.
const WRITE_VERB: &str = "write";
const WRITE_EVENT: &str = "revision_written";

impl OrbitRuntime {
    pub(super) fn crew_memory_store(&self) -> Result<Arc<dyn CrewMemoryStoreBackend>, OrbitError> {
        Ok(orbit_store::compose::crew_memory_store(
            self.sqlite_store()?,
        ))
    }

    /// An operator-named key. A configured crew's name is refused with the
    /// model it runs, since a space belongs to the model, not the crew.
    fn operator_memory_key(&self, raw: &str) -> Result<MemoryKey, OrbitError> {
        let key = parse_memory_key(raw)?;
        if key.is_global() {
            return Ok(key);
        }
        let crews = self.context.settings().crews();
        if let Some(crew) = crews
            .values()
            .find(|crew| crew.name == key.as_str() || crew.base_name() == key.as_str())
            .filter(|crew| crew.assignment.model != key.as_str())
        {
            return Err(OrbitError::InvalidInput(format!(
                "`{key}` is a crew; crew memory is keyed by model, so name its model `{}`",
                crew.assignment.model
            )));
        }
        Ok(key)
    }

    pub(super) fn operator_memory_writer(&self) -> MemoryWriter {
        let label = self.actor_label().trim();
        MemoryWriter::Operator {
            actor: if label.is_empty() { "operator" } else { label }.to_string(),
        }
    }

    /// The score-blind lint's names for a write to `own` keys: every
    /// scorecard term, and every configured crew name and model except the
    /// ones of `own`.
    pub(super) fn crew_memory_vocabulary(&self, own: &[&MemoryKey]) -> MemoryLintVocabulary {
        let crews = self.context.settings().crews();
        let mut own_names = BTreeSet::new();
        let mut other_names = BTreeSet::new();
        for crew in crews.values() {
            let model = crew.assignment.model.trim();
            let names = [crew.name.as_str(), crew.base_name(), model];
            let target = if own.iter().any(|key| key.as_str() == model) {
                &mut own_names
            } else {
                &mut other_names
            };
            target.extend(
                names
                    .into_iter()
                    .map(str::to_lowercase)
                    .filter(|name| name.chars().count() >= 3),
            );
        }
        own_names.extend(own.iter().map(|key| key.as_str().to_lowercase()));
        MemoryLintVocabulary {
            scorecard_terms: crate::application::agents::scorecard_term_vocabulary()
                .into_iter()
                .filter(|term| term.contains(['_', ' ']))
                .map(str::to_string)
                .collect(),
            other_names: other_names.difference(&own_names).cloned().collect(),
        }
    }

    /// Refuse `verb` on `key` unless a running experiment has a memory arm,
    /// read from the store each time so concluding it takes effect at once.
    pub(super) fn ensure_crew_memory_writes_enabled(
        &self,
        verb: &str,
        key: &MemoryKey,
    ) -> Result<(), OrbitError> {
        if self.crew_memory_experiment_running() {
            return Ok(());
        }
        Err(OrbitError::PolicyDenied(format!(
            "crew memory {verb} refused for `{key}`: crew memory is off in this workspace, as no running experiment has a memory arm"
        )))
    }

    /// The one crew memory write path. Refused unless a running experiment
    /// has a memory arm; otherwise runs [`validate_memory_write`] and appends a
    /// revision, or records the refusal and refuses the whole write. A
    /// dreaming run's write also cites only its evidence pack.
    pub(super) fn write_crew_memory_revision(
        &self,
        key: &MemoryKey,
        mut write: MemoryWrite,
        writer: &MemoryWriter,
        reason: Option<String>,
        dreaming: Option<&DreamingWrite<'_>>,
    ) -> Result<Value, OrbitError> {
        self.ensure_crew_memory_writes_enabled(WRITE_VERB, key)?;
        self.ensure_coordination_task_write_permitted()?;
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let vocabulary = self.crew_memory_vocabulary(&[key]);
        let context = MemoryWriteContext {
            key,
            writer,
            vocabulary: &vocabulary,
            evidence_pack: dreaming.map(|run| run.evidence_pack),
        };
        let task_id = dreaming.map(|run| run.task_id);
        // Stored at microsecond precision; truncate so the reply matches a read.
        let now = Utc::now().trunc_subsecs(6);
        if let Err(refusal) = validate_memory_write(&write, &context) {
            store.append_crew_memory_event(&CrewMemoryEventRow {
                workspace_id,
                memory_key: key.to_string(),
                at: now,
                action: "write_refused".to_string(),
                actor: writer.actor().to_string(),
                revision: None,
                detail_json: json!({
                    "verb": WRITE_VERB,
                    "task_id": task_id,
                    "rule": refusal.rule,
                    "file": refusal.file,
                    "token": refusal.token,
                    "message": refusal.message,
                })
                .to_string(),
            })?;
            return Err(OrbitError::InvalidInput(refusal.to_string()));
        }
        write.files.sort_by(|a, b| a.path.cmp(&b.path));
        let (run_id, crew) = match writer {
            MemoryWriter::Operator { .. } => (None, None),
            MemoryWriter::Run { run_id, crew, .. } => (Some(run_id.clone()), Some(crew.clone())),
        };
        let row = CrewMemoryRevisionRow {
            workspace_id: workspace_id.clone(),
            memory_key: key.to_string(),
            revision: 0,
            files_json: json_text(&write.files)?,
            content_digest: content_digest(&write.files)?,
            evidence_json: json_text(&write.evidence)?,
            writer_kind: writer.kind().to_string(),
            writer_actor: writer.actor().to_string(),
            writer_run_id: run_id,
            writer_crew: crew,
            reason: reason.clone(),
            created_at: now,
        };
        let mut detail = json!({ "verb": WRITE_VERB, "reason": reason });
        if let Some(task_id) = task_id {
            detail["task_id"] = json!(task_id);
        }
        let event = CrewMemoryEventRow {
            workspace_id: workspace_id.clone(),
            memory_key: key.to_string(),
            at: now,
            action: WRITE_EVENT.to_string(),
            actor: writer.actor().to_string(),
            revision: None,
            detail_json: detail.to_string(),
        };
        let stored = store.append_crew_memory_revision(&row, &event)?;
        let mut view = revision_view(key, Some(&stored))?;
        view["action"] = json!(WRITE_EVENT);
        Ok(view)
    }

    /// One space's newest revision, or `revision`; without a key, every
    /// space's current summary.
    pub fn crew_memory_show(
        &self,
        key: Option<&str>,
        revision: Option<u32>,
    ) -> Result<Value, OrbitError> {
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let enabled = self.crew_memory_experiment_running();
        let Some(raw) = key else {
            if revision.is_some() {
                return Err(OrbitError::InvalidInput(
                    "`revision` needs a memory key".to_string(),
                ));
            }
            let mut spaces = self.crew_memory_space_rows()?;
            // While crew memory is off, only stored spaces are listed.
            if !enabled {
                spaces.retain(|space| !space["revision"].is_null());
            }
            return Ok(json!({ "enabled": enabled, "count": spaces.len(), "spaces": spaces }));
        };
        let key = self.operator_memory_key(raw)?;
        let row = store.crew_memory_revision(&workspace_id, key.as_str(), revision)?;
        if let (Some(number), None) = (revision, &row) {
            return Err(OrbitError::InvalidInput(format!(
                "crew memory `{key}` has no revision {number}"
            )));
        }
        let mut view = revision_view(&key, row.as_ref())?;
        view["enabled"] = json!(enabled);
        Ok(view)
    }

    /// The space summary rows `orbit.memory.show` with no key lists, as a
    /// read that writes nothing: `global` first, then
    /// each key with a stored revision by key, then each configured crew's
    /// model key and each proposing key with none yet. Every row has one
    /// shape: its current revision's usage against the key's caps, its
    /// writer and its open proposal count. Open proposals are read only
    /// while a running experiment has a memory arm.
    pub(super) fn crew_memory_space_rows(&self) -> Result<Vec<Value>, OrbitError> {
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let current = store.crew_memory_current(&workspace_id)?;
        let open = if self.crew_memory_experiment_running() {
            self.unexpired_crew_memory_proposals()?
        } else {
            Vec::new()
        };
        let mut keys: BTreeSet<MemoryKey> = BTreeSet::new();
        for row in &current {
            keys.insert(parse_memory_key(&row.memory_key)?);
        }
        for crew in self.context.settings().crews().values() {
            if let Some(key) = super::memory_key(Some(crew.assignment.model.as_str())) {
                keys.insert(key);
            }
        }
        for proposal in &open {
            keys.insert(parse_memory_key(&proposal.proposing_key)?);
        }
        let global = MemoryKey::global();
        keys.remove(&global);
        let (written, empty): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|key| current.iter().any(|row| row.memory_key == key.as_str()));
        std::iter::once(global)
            .chain(written)
            .chain(empty)
            .map(|key| {
                let row = current.iter().find(|row| row.memory_key == key.as_str());
                let mut view = revision_view(&key, row)?;
                if let Some(object) = view.as_object_mut() {
                    object.remove("files");
                    object.remove("evidence");
                    object.remove("reason");
                }
                view["open_proposals"] = json!(
                    open.iter()
                        .filter(|proposal| proposal.proposing_key == key.as_str())
                        .count()
                );
                Ok(view)
            })
            .collect()
    }

    /// Replace a space's files with a new revision, as the operator. No
    /// operator surface calls it while crew memory is dormant; it is the
    /// write an experiment variant's surface would reach.
    pub fn crew_memory_write(
        &self,
        key: &str,
        write: MemoryWrite,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        let key = self.operator_memory_key(key)?;
        let writer = self.operator_memory_writer();
        self.write_crew_memory_revision(&key, write, &writer, reason, None)
    }
}

pub(super) fn json_text<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value)
        .map_err(|error| OrbitError::Execution(format!("serialize crew memory: {error}")))
}

/// `sha256:<hex>` over the files' canonical JSON, sorted by path.
fn content_digest(files: &[MemoryFile]) -> Result<String, OrbitError> {
    let digest = Sha256::digest(json_text(&files)?.as_bytes());
    Ok(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

pub(super) fn crew_memory_revision_files(
    row: &CrewMemoryRevisionRow,
) -> Result<Vec<MemoryFile>, OrbitError> {
    serde_json::from_str(&row.files_json).map_err(|error| {
        OrbitError::Store(format!(
            "invalid crew memory revision {} of `{}`: {error}",
            row.revision, row.memory_key
        ))
    })
}

pub(super) fn crew_memory_revision_evidence(
    row: &CrewMemoryRevisionRow,
) -> Result<Vec<String>, OrbitError> {
    serde_json::from_str(&row.evidence_json).map_err(|error| {
        OrbitError::Store(format!(
            "invalid crew memory evidence on revision {} of `{}`: {error}",
            row.revision, row.memory_key
        ))
    })
}

fn byte_count(files: &[MemoryFile]) -> usize {
    files.iter().map(|file| file.text.len()).sum()
}

/// A space's revision as the CLI and tools show it; an empty space when the
/// key has no revision yet.
pub(super) fn revision_view(
    key: &MemoryKey,
    row: Option<&CrewMemoryRevisionRow>,
) -> Result<Value, OrbitError> {
    let (max_files, max_bytes) = if key.is_global() {
        (GLOBAL_MAX_FILES, GLOBAL_MAX_BYTES)
    } else {
        (MODEL_SPACE_MAX_FILES, MODEL_SPACE_MAX_BYTES)
    };
    let caps = json!({ "max_files": max_files, "max_bytes": max_bytes });
    let Some(row) = row else {
        return Ok(json!({
            "key": key,
            "revision": null,
            "files": [],
            "file_count": 0,
            "bytes": 0,
            "digest": null,
            "evidence": [],
            "writer": null,
            "reason": null,
            "created_at": null,
            "caps": caps,
        }));
    };
    let files = crew_memory_revision_files(row)?;
    Ok(json!({
        "key": key,
        "revision": row.revision,
        "file_count": files.len(),
        "bytes": byte_count(&files),
        "files": files,
        "digest": row.content_digest,
        "evidence": crew_memory_revision_evidence(row)?,
        "writer": {
            "kind": row.writer_kind,
            "actor": row.writer_actor,
            "run_id": row.writer_run_id,
            "crew": row.writer_crew,
        },
        "reason": row.reason,
        "created_at": row.created_at,
        "caps": caps,
    }))
}
