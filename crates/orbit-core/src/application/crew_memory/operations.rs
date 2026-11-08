//! The operator's crew memory operations and the one write path every
//! writer shares.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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

/// The policy key that gates every crew memory write.
const ENABLED_KEY: &str = "crew_memory.enabled";

/// What a revision records about the change that wrote it.
enum WriteAction {
    Write,
    Reset,
    Copy { from: MemoryKey, from_revision: u32 },
}

impl WriteAction {
    fn verb(&self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Reset => "reset",
            Self::Copy { .. } => "copy",
        }
    }

    /// The audit action of the revision this change writes.
    fn event(&self) -> &'static str {
        match self {
            Self::Write => "revision_written",
            Self::Reset => "reset",
            Self::Copy { .. } => "copied",
        }
    }
}

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

    fn operator_memory_writer(&self) -> MemoryWriter {
        let label = self.actor_label().trim();
        MemoryWriter::Operator {
            actor: if label.is_empty() { "operator" } else { label }.to_string(),
        }
    }

    /// The score-blind lint's names for a write to `own` keys: every
    /// scorecard term, and every configured crew name and model except the
    /// ones of `own`.
    fn crew_memory_vocabulary(&self, own: &[&MemoryKey]) -> MemoryLintVocabulary {
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

    /// Refuse `verb` on `key` while `crew_memory.enabled` is false, read
    /// from the current config so a disable takes effect at once.
    fn ensure_crew_memory_writes_enabled(
        &self,
        verb: &str,
        key: &MemoryKey,
    ) -> Result<(), OrbitError> {
        if self.current_crew_memory_enabled() {
            return Ok(());
        }
        Err(OrbitError::PolicyDenied(format!(
            "crew memory {verb} refused for `{key}`: crew memory is disabled in this workspace (`{ENABLED_KEY}` is false)"
        )))
    }

    /// The one crew memory write path. Refused while `crew_memory.enabled`
    /// is false; otherwise runs [`validate_memory_write`] and appends a
    /// revision, or records the refusal and refuses the whole write.
    fn write_crew_memory_revision(
        &self,
        key: &MemoryKey,
        mut write: MemoryWrite,
        writer: &MemoryWriter,
        action: WriteAction,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        self.ensure_crew_memory_writes_enabled(action.verb(), key)?;
        self.ensure_coordination_task_write_permitted()?;
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let mut own = vec![key];
        if let WriteAction::Copy { from, .. } = &action {
            own.push(from);
        }
        let vocabulary = self.crew_memory_vocabulary(&own);
        let context = MemoryWriteContext {
            key,
            writer,
            vocabulary: &vocabulary,
        };
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
                    "verb": action.verb(),
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
        let mut detail = json!({ "verb": action.verb(), "reason": reason });
        if let WriteAction::Copy {
            from,
            from_revision,
        } = &action
        {
            detail["from"] = json!(from);
            detail["from_revision"] = json!(from_revision);
        }
        let stored = store.append_crew_memory_revision(
            &row,
            &CrewMemoryEventRow {
                workspace_id,
                memory_key: key.to_string(),
                at: now,
                action: action.event().to_string(),
                actor: writer.actor().to_string(),
                revision: None,
                detail_json: detail.to_string(),
            },
        )?;
        let mut view = revision_view(key, Some(&stored))?;
        view["action"] = json!(action.event());
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
        let enabled = self.current_crew_memory_enabled();
        let Some(raw) = key else {
            if revision.is_some() {
                return Err(OrbitError::InvalidInput(
                    "`revision` needs a memory key".to_string(),
                ));
            }
            let spaces = store
                .crew_memory_current(&workspace_id)?
                .iter()
                .map(|row| {
                    let files = parse_revision_files(row)?;
                    Ok(json!({
                        "key": row.memory_key,
                        "revision": row.revision,
                        "file_count": files.len(),
                        "bytes": byte_count(&files),
                        "digest": row.content_digest,
                        "created_at": row.created_at,
                    }))
                })
                .collect::<Result<Vec<_>, OrbitError>>()?;
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

    /// One space's revisions, newest first, and its audit events in order.
    pub fn crew_memory_history(&self, key: &str) -> Result<Value, OrbitError> {
        let key = self.operator_memory_key(key)?;
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let revisions = store
            .crew_memory_revisions(&workspace_id, key.as_str())?
            .iter()
            .map(|row| {
                let mut view = revision_view(&key, Some(row))?;
                let paths: Vec<Value> = view["files"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|file| file["path"].clone())
                    .collect();
                view["files"] = Value::Array(paths);
                if let Some(object) = view.as_object_mut() {
                    object.remove("key");
                    object.remove("caps");
                }
                Ok(view)
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        let events = store
            .crew_memory_events(&workspace_id, key.as_str())?
            .into_iter()
            .map(|event| {
                Ok(json!({
                    "at": event.at,
                    "action": event.action,
                    "actor": event.actor,
                    "revision": event.revision,
                    "detail": serde_json::from_str::<Value>(&event.detail_json)
                        .map_err(|error| OrbitError::Store(format!("invalid crew memory event: {error}")))?,
                }))
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        Ok(json!({ "key": key, "revisions": revisions, "events": events }))
    }

    /// Render every current space as `global.md` and `<model>/<file>.md`
    /// under `dir` (default: the workspace's `.orbit/memory`). Orbit never
    /// reads these files back. An empty space writes nothing.
    pub fn crew_memory_export(&self, dir: Option<&Path>) -> Result<Value, OrbitError> {
        let dir = dir.map_or_else(|| self.shared_root().join("memory"), Path::to_path_buf);
        let store = self.crew_memory_store()?;
        let rows = store.crew_memory_current(&self.workspace_id()?)?;
        let mut written = Vec::new();
        for row in &rows {
            let key = parse_memory_key(&row.memory_key)?;
            let files = parse_revision_files(row)?;
            if key.is_global() {
                if let Some(file) = files.first() {
                    write_export(&dir, &PathBuf::from("global.md"), &file.text)?;
                    written.push("global.md".to_string());
                }
                continue;
            }
            for file in &files {
                let relative = Path::new(&key.export_dir_name()).join(&file.path);
                write_export(&dir, &relative, &file.text)?;
                written.push(relative.to_string_lossy().into_owned());
            }
        }
        Ok(json!({
            "dir": dir.to_string_lossy(),
            "spaces": rows.len(),
            "files": written,
        }))
    }

    /// Replace a space's files with a new revision, as the operator.
    pub fn crew_memory_write(
        &self,
        key: &str,
        write: MemoryWrite,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        let key = self.operator_memory_key(key)?;
        let writer = self.operator_memory_writer();
        self.write_crew_memory_revision(&key, write, &writer, WriteAction::Write, reason)
    }

    /// Empty a space with a new revision that holds no files.
    pub fn crew_memory_reset(
        &self,
        key: &str,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        let key = self.operator_memory_key(key)?;
        let writer = self.operator_memory_writer();
        self.write_crew_memory_revision(
            &key,
            MemoryWrite::default(),
            &writer,
            WriteAction::Reset,
            reason,
        )
    }

    /// Copy `from`'s current space into `to` as a new revision, keeping the
    /// source revision's evidence.
    pub fn crew_memory_copy(
        &self,
        from: &str,
        to: &str,
        reason: Option<String>,
    ) -> Result<Value, OrbitError> {
        let from = self.operator_memory_key(from)?;
        let to = self.operator_memory_key(to)?;
        if from.is_global() || to.is_global() {
            return Err(OrbitError::InvalidInput(
                "crew memory copy moves one model's space to another model; `global` is neither"
                    .to_string(),
            ));
        }
        if from == to {
            return Err(OrbitError::InvalidInput(format!(
                "crew memory copy needs two different models; both are `{from}`"
            )));
        }
        // Refuse before the source is read, so a disabled workspace answers
        // the same whatever the source holds.
        self.ensure_crew_memory_writes_enabled("copy", &to)?;
        let writer = self.operator_memory_writer();
        let source = self
            .crew_memory_store()?
            .crew_memory_revision(&self.workspace_id()?, from.as_str(), None)?
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("crew memory `{from}` has no revision to copy"))
            })?;
        let write = MemoryWrite {
            files: parse_revision_files(&source)?,
            evidence: parse_evidence(&source)?,
        };
        self.write_crew_memory_revision(
            &to,
            write,
            &writer,
            WriteAction::Copy {
                from,
                from_revision: source.revision,
            },
            reason,
        )
    }
}

fn json_text<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
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

pub(super) fn parse_revision_files(
    row: &CrewMemoryRevisionRow,
) -> Result<Vec<MemoryFile>, OrbitError> {
    serde_json::from_str(&row.files_json).map_err(|error| {
        OrbitError::Store(format!(
            "invalid crew memory revision {} of `{}`: {error}",
            row.revision, row.memory_key
        ))
    })
}

fn parse_evidence(row: &CrewMemoryRevisionRow) -> Result<Vec<String>, OrbitError> {
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
fn revision_view(
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
    let files = parse_revision_files(row)?;
    Ok(json!({
        "key": key,
        "revision": row.revision,
        "file_count": files.len(),
        "bytes": byte_count(&files),
        "files": files,
        "digest": row.content_digest,
        "evidence": parse_evidence(row)?,
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

fn write_export(dir: &Path, relative: &Path, text: &str) -> Result<(), OrbitError> {
    let path = dir.join(relative);
    let failed = |error: std::io::Error| {
        OrbitError::Execution(format!("export crew memory to {}: {error}", path.display()))
    };
    if let Some(parent) = path.parent() {
        orbit_common::fs::io::create_private_dir_all(parent).map_err(failed)?;
    }
    orbit_common::fs::io::atomic_write_text(&path, text).map_err(failed)
}
