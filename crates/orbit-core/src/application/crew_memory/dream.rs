//! Dreaming (slice M3, design §6): the weekly `crew_memory_dream` routine
//! mints one reflection task per model with enough recent implementer work,
//! each carrying its frozen, score-blind evidence pack, and records what an
//! earlier week's dreaming run left behind.
//!
//! The routine mints nothing while `crew_memory.enabled` is false or while
//! no running (shadow or live) experiment in the workspace has a memory arm.
//! A key with an open dreaming task, with no enabled crew on its model, or
//! whose pack holds no evidence a write could cite is skipped, and the skip
//! is recorded in the key's audit. The mint's `dream_minted` event holds the pack's task
//! ids, which is what a dreaming run's write and proposals may cite: the
//! artifact is the run's to read, the event is Orbit's record of it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::CrewMemoryEventRow;
use orbit_types::task::{
    ExternalRef, Task, TaskArtifact, TaskComplexity, TaskOrigin, TaskPriority, TaskStatus, TaskType,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::operations::ENABLED_KEY;
use super::validate::strip_score_blind_tokens;
use super::{MemoryKey, MemoryLintVocabulary, memory_key};
use crate::OrbitRuntime;
use crate::application::agents::memory_evidence::{
    EvidenceSection, ModelEvidence, implementer_units_by_model, model_evidence,
};
use crate::application::task::{TaskAddParams, TaskRecordUpdateParams, TaskRecords};

/// The tag every dreaming task carries.
pub(crate) const CREW_MEMORY_DREAM_TAG: &str = "crew-memory-dream";
/// The external reference system that carries a dreaming task's
/// `crew_memory_key`: its id is the model key the task reflects on.
pub(crate) const CREW_MEMORY_KEY_REF_SYSTEM: &str = "crew-memory-key";
/// The task artifact holding a dreaming task's frozen evidence pack.
pub(crate) const CREW_MEMORY_EVIDENCE_ARTIFACT: &str = "crew-memory-evidence.json";
/// Implementer units in the trailing 7 days a model needs to dream.
pub(crate) const CREW_MEMORY_DREAM_MIN_SUPPORT: usize = 10;
/// The evidence pack's size cap.
pub(crate) const CREW_MEMORY_EVIDENCE_MAX_BYTES: usize = 64 * 1024;
/// The agent tools a dreaming task's implementer is granted.
pub(crate) const CREW_MEMORY_AGENT_TOOLS: &[&str] = &[
    "orbit.memory.show",
    "orbit.memory.write",
    "orbit.memory.propose",
];

const NO_DIFF_TAG: &str = "no-diff-expected";
const PACK_SCHEMA_VERSION: u32 = 1;

/// Audit actions the routine records on a key.
pub(super) const DREAM_MINTED: &str = "dream_minted";
const DREAM_SKIPPED: &str = "dream_skipped";
const NO_REVISION: &str = "no_revision";
const REVISION_WRITTEN: &str = "revision_written";
/// The routine's audit actor.
const ROUTINE_ACTOR: &str = "crew_memory_dream";

/// The model key a dreaming task reflects on: its one `crew-memory-key`
/// reference, on a task tagged `crew-memory-dream`. `None` for every other
/// task.
pub(crate) fn dreaming_task_key(task: &Task) -> Option<MemoryKey> {
    if !task.tags.iter().any(|tag| tag == CREW_MEMORY_DREAM_TAG) {
        return None;
    }
    let mut refs = task
        .external_refs
        .iter()
        .filter(|reference| reference.system == CREW_MEMORY_KEY_REF_SYSTEM);
    let reference = refs.next()?;
    if refs.next().is_some() {
        return None;
    }
    memory_key(Some(&reference.id))
}

/// `2026-W45`: the ISO week a mint belongs to.
fn iso_week(at: DateTime<Utc>) -> String {
    let week = at.iso_week();
    format!("{}-W{:02}", week.year(), week.week())
}

fn key_reference(key: &MemoryKey) -> ExternalRef {
    ExternalRef {
        system: CREW_MEMORY_KEY_REF_SYSTEM.to_string(),
        id: key.to_string(),
        url: None,
    }
}

impl OrbitRuntime {
    /// One pass of the `crew_memory_dream` routine. Returns what it minted,
    /// skipped and settled; the gate's reason when it did nothing.
    pub fn crew_memory_dream(&self) -> Result<Value, OrbitError> {
        let now = Utc::now();
        let week = iso_week(now);
        if !self.current_crew_memory_enabled() {
            return Ok(json!({
                "week": week,
                "minted": [],
                "skipped_reason": "disabled",
                "message": format!("crew memory is disabled in this workspace (`{ENABLED_KEY}` is false); nothing was minted"),
            }));
        }
        let experiments = self.running_memory_experiments()?;
        if experiments.is_empty() {
            return Ok(json!({
                "week": week,
                "minted": [],
                "skipped_reason": "no_memory_experiment",
                "message": "no running shadow or live experiment in this workspace has a memory arm; nothing was minted",
            }));
        }
        self.ensure_coordination_task_write_permitted()?;
        let no_revision = self.settle_dreaming_tasks(now)?;
        let facts = self.agent_scan()?;
        let mut minted = Vec::new();
        let mut skipped = Vec::new();
        let mut below_support = Vec::new();
        for (model, units) in implementer_units_by_model(&facts)? {
            let Some(key) = memory_key(Some(&model)) else {
                continue;
            };
            if units < CREW_MEMORY_DREAM_MIN_SUPPORT {
                below_support.push(json!({ "key": key, "units": units }));
                continue;
            }
            if let Some(open) = self.open_dreaming_task(&key)? {
                self.record_dream_skip(&key, &week, now, "open_task", Some(&open.id))?;
                skipped.push(json!({ "key": key, "reason": "open_task", "task_id": open.id }));
                continue;
            }
            let Some(crew) = self.crew_memory_base_crew(&key) else {
                self.record_dream_skip(&key, &week, now, "no_base_crew", None)?;
                skipped.push(json!({ "key": key, "reason": "no_base_crew" }));
                continue;
            };
            let vocabulary = self.crew_memory_vocabulary(&[&key]);
            let pack = render_evidence_pack(
                &key,
                &week,
                now,
                model_evidence(&facts, &model)?,
                &vocabulary,
            )?;
            // A write must cite evidence from the pack, so a pack with none
            // leaves the run nothing it could write: skip rather than spend
            // a run on it.
            if pack.evidence_ids.is_empty() {
                self.record_dream_skip(&key, &week, now, "no_evidence", None)?;
                skipped.push(json!({ "key": key, "reason": "no_evidence" }));
                continue;
            }
            let task = self.mint_dreaming_task(&key, &crew, &pack)?;
            self.crew_memory_store()?
                .append_crew_memory_event(&CrewMemoryEventRow {
                    workspace_id: self.workspace_id()?,
                    memory_key: key.to_string(),
                    at: now,
                    action: DREAM_MINTED.to_string(),
                    actor: ROUTINE_ACTOR.to_string(),
                    revision: None,
                    detail_json: json!({
                        "task_id": task.id,
                        "week": week,
                        "crew": crew,
                        "evidence_ids": pack.evidence_ids,
                        "pack_digest": pack.digest,
                        "pack_bytes": pack.bytes.len(),
                        "dropped_tasks": pack.dropped_tasks,
                        "dropped_occurrences": pack.dropped_occurrences,
                    })
                    .to_string(),
                })?;
            minted.push(json!({
                "key": key,
                "task_id": task.id,
                "crew": crew,
                "evidence_ids": pack.evidence_ids.len(),
                "pack_bytes": pack.bytes.len(),
                "dropped_occurrences": pack.dropped_occurrences,
            }));
        }
        Ok(json!({
            "week": week,
            "experiments": experiments,
            "min_support": CREW_MEMORY_DREAM_MIN_SUPPORT,
            "minted": minted,
            "skipped": skipped,
            "below_support": below_support,
            "no_revision": no_revision,
        }))
    }

    /// The open (non-terminal) dreaming task for `key`, if any.
    fn open_dreaming_task(&self, key: &MemoryKey) -> Result<Option<Task>, OrbitError> {
        Ok(self
            .list_tasks_filtered(None, None, None, None, Some(&key_reference(key)), None)?
            .into_iter()
            .find(|task| {
                !task.status.is_terminal() && dreaming_task_key(task).as_ref() == Some(key)
            }))
    }

    /// The crew a model's dreaming task runs on: an enabled crew of that
    /// model at its own effort, else the one at the lowest configured
    /// effort, by name on a tie. `None` when no enabled crew runs the model.
    pub(super) fn crew_memory_base_crew(&self, key: &MemoryKey) -> Option<String> {
        self.context
            .settings()
            .crews()
            .values()
            .filter(|crew| crew.enabled && crew.assignment.model.trim() == key.as_str())
            .min_by_key(|crew| {
                (
                    crew.assignment.effort.is_some(),
                    crew.assignment.effort,
                    crew.name.clone(),
                )
            })
            .map(|crew| crew.name.clone())
    }

    fn record_dream_skip(
        &self,
        key: &MemoryKey,
        week: &str,
        now: DateTime<Utc>,
        reason: &str,
        task_id: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.crew_memory_store()?
            .append_crew_memory_event(&CrewMemoryEventRow {
                workspace_id: self.workspace_id()?,
                memory_key: key.to_string(),
                at: now,
                action: DREAM_SKIPPED.to_string(),
                actor: ROUTINE_ACTOR.to_string(),
                revision: None,
                detail_json: json!({ "week": week, "reason": reason, "task_id": task_id })
                    .to_string(),
            })
    }

    /// Record `no_revision` for each finished dreaming task whose run wrote
    /// no revision of its key, once. Returns what it recorded.
    fn settle_dreaming_tasks(&self, now: DateTime<Utc>) -> Result<Vec<Value>, OrbitError> {
        let mut finished: BTreeMap<MemoryKey, Vec<Task>> = BTreeMap::new();
        for task in self.list_tasks_by_tags(&[CREW_MEMORY_DREAM_TAG.to_string()])? {
            if task.status.is_terminal()
                && let Some(key) = dreaming_task_key(&task)
            {
                finished.entry(key).or_default().push(task);
            }
        }
        let store = self.crew_memory_store()?;
        let workspace_id = self.workspace_id()?;
        let mut recorded = Vec::new();
        for (key, tasks) in finished {
            let events = store.crew_memory_events(&workspace_id, key.as_str())?;
            let mut minted_week: BTreeMap<String, String> = BTreeMap::new();
            let mut settled: BTreeSet<String> = BTreeSet::new();
            for event in &events {
                let detail: Value = serde_json::from_str(&event.detail_json).unwrap_or(Value::Null);
                let Some(task_id) = detail["task_id"].as_str() else {
                    continue;
                };
                match event.action.as_str() {
                    DREAM_MINTED => {
                        minted_week.insert(
                            task_id.to_string(),
                            detail["week"].as_str().unwrap_or_default().to_string(),
                        );
                    }
                    REVISION_WRITTEN | NO_REVISION => {
                        settled.insert(task_id.to_string());
                    }
                    _ => {}
                }
            }
            for task in tasks {
                let Some(week) = minted_week.get(task.id.as_str()) else {
                    continue;
                };
                if settled.contains(task.id.as_str()) {
                    continue;
                }
                store.append_crew_memory_event(&CrewMemoryEventRow {
                    workspace_id: workspace_id.clone(),
                    memory_key: key.to_string(),
                    at: now,
                    action: NO_REVISION.to_string(),
                    actor: ROUTINE_ACTOR.to_string(),
                    revision: None,
                    detail_json: json!({
                        "task_id": task.id,
                        "week": week,
                        "status": task.status.to_string(),
                    })
                    .to_string(),
                })?;
                recorded.push(json!({ "key": key, "task_id": task.id, "week": week }));
            }
        }
        Ok(recorded)
    }

    /// Mint the dreaming task for `key` on `crew` to `backlog` and attach
    /// its evidence pack.
    fn mint_dreaming_task(
        &self,
        key: &MemoryKey,
        crew: &str,
        pack: &RenderedPack,
    ) -> Result<Task, OrbitError> {
        let task = self.add_task(TaskAddParams {
            title: format!("Crew memory: reflect on {key}'s week"),
            description: dreaming_description(key),
            acceptance_criteria: vec![
                "Read the current memory space with orbit.memory.show and the task artifact crew-memory-evidence.json.".to_string(),
                "Either rewrite the space whole with one orbit.memory.write that cites, for each note, evidence task ids from the pack, or leave the space unchanged.".to_string(),
                "Open at most 2 orbit.memory.propose proposals for the global memory, each citing evidence task ids from the pack.".to_string(),
            ],
            tags: vec![CREW_MEMORY_DREAM_TAG.to_string(), NO_DIFF_TAG.to_string()],
            // The grant keeps the task on the owner, whose store holds the
            // memory; the tools themselves are admitted per call.
            granted_tools: CREW_MEMORY_AGENT_TOOLS
                .iter()
                .map(|tool| (*tool).to_string())
                .collect(),
            priority: TaskPriority::Low,
            complexity: TaskComplexity::Low,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            system_created: true,
            external_refs: vec![key_reference(key)],
            crew: Some(crew.to_string()),
            origin: Some(TaskOrigin::Automation),
            ..TaskAddParams::default()
        })?;
        let actor = self.actor().resolve_write_label(None, None)?;
        self.stores()
            .task_records()
            .update(
                &task.id,
                TaskRecordUpdateParams {
                    actor,
                    upsert_artifacts: vec![TaskArtifact {
                        path: CREW_MEMORY_EVIDENCE_ARTIFACT.to_string(),
                        content: pack.bytes.clone(),
                        media_type: "application/json".to_string(),
                        created_by: None,
                    }],
                    ..Default::default()
                },
            )
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "dreaming task {} was created but {CREW_MEMORY_EVIDENCE_ARTIFACT} was not attached: {error}",
                    task.id
                ))
            })?;
        Ok(task)
    }
}

fn dreaming_description(key: &MemoryKey) -> String {
    format!(
        "Reflect on the work model `{key}` did in this repository in the past week, and keep \
         its crew memory space current. The space holds notes this model wrote about its own \
         past work; they reach later tasks as data, never as instructions.\n\n\
         1. Read the space with `orbit.memory.show` (its key defaults to `{key}`) and the \
         evidence pack, task artifact `crew-memory-evidence.json`.\n\
         2. Look for recurring causes in the work the pack describes, not for patterns in how \
         often things happened.\n\
         3. Rewrite the space whole with one `orbit.memory.write` (key `{key}`), citing the \
         evidence task ids each note rests on; only ids from the pack's `evidence_ids` are \
         accepted. A note with no new evidence may be kept for at most 4 weekly revisions and \
         must say how old it is. If nothing is worth changing, write nothing: the space stays \
         as it is and the week is recorded as `no_revision`.\n\
         4. Optionally open up to 2 `orbit.memory.propose` proposals for the global memory \
         every model receives, each citing evidence from the pack.\n\n\
         A write is refused whole, naming the rule, when it exceeds 8 files or 6 KiB, cites \
         no evidence, uses scorecard vocabulary or another crew's or model's name, or contains \
         a URL, a tool name, a shell command block or an instruction to the agent harness. \
         The revision and proposal rows are this task's output: there is no code change, and \
         the final message is advisory."
    )
}

/// An evidence pack rendered for the artifact.
pub(super) struct RenderedPack {
    pub(super) bytes: Vec<u8>,
    pub(super) digest: String,
    pub(super) evidence_ids: Vec<String>,
    pub(super) dropped_tasks: usize,
    pub(super) dropped_occurrences: usize,
}

/// Render `evidence` as the pack (design §6.2): free text stripped of the
/// score-blind lint's tokens, capped at
/// [`CREW_MEMORY_EVIDENCE_MAX_BYTES`] by dropping the oldest task of the
/// problem with the most tasks until it fits, and stating what it dropped.
pub(super) fn render_evidence_pack(
    key: &MemoryKey,
    week: &str,
    now: DateTime<Utc>,
    mut evidence: ModelEvidence,
    vocabulary: &MemoryLintVocabulary,
) -> Result<RenderedPack, OrbitError> {
    for section in sections_mut(&mut evidence) {
        for problem in &mut section.problems {
            for task in &mut problem.tasks {
                task.title = strip_score_blind_tokens(&task.title, vocabulary);
                for occurrence in &mut task.occurrences {
                    occurrence.detail = strip_score_blind_tokens(&occurrence.detail, vocabulary);
                }
                for fix in &mut task.regression_fixes {
                    fix.title = strip_score_blind_tokens(&fix.title, vocabulary);
                }
            }
        }
    }
    let mut dropped_tasks = 0usize;
    let mut dropped_occurrences = 0usize;
    loop {
        let evidence_ids = evidence_ids(&evidence);
        let value = pack_value(
            key,
            week,
            now,
            &evidence,
            &evidence_ids,
            dropped_tasks,
            dropped_occurrences,
        );
        let bytes = serde_json::to_vec_pretty(&value).map_err(|error| {
            OrbitError::Execution(format!("serialize crew memory evidence: {error}"))
        })?;
        if bytes.len() <= CREW_MEMORY_EVIDENCE_MAX_BYTES {
            let digest = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            return Ok(RenderedPack {
                bytes,
                digest: format!("sha256:{digest}"),
                evidence_ids: evidence_ids.into_iter().collect(),
                dropped_tasks,
                dropped_occurrences,
            });
        }
        let Some(problem) = sections_mut(&mut evidence)
            .into_iter()
            .flat_map(|section| section.problems.iter_mut())
            .filter(|problem| !problem.tasks.is_empty())
            .max_by_key(|problem| problem.tasks.len())
        else {
            return Err(OrbitError::Execution(format!(
                "crew memory evidence for `{key}` exceeds {CREW_MEMORY_EVIDENCE_MAX_BYTES} bytes with every occurrence dropped"
            )));
        };
        if let Some(task) = problem.tasks.pop() {
            dropped_tasks += 1;
            dropped_occurrences += task.occurrences.len().max(1);
        }
        for section in sections_mut(&mut evidence) {
            section.problems.retain(|problem| !problem.tasks.is_empty());
        }
    }
}

fn sections_mut(evidence: &mut ModelEvidence) -> [&mut EvidenceSection; 3] {
    [
        &mut evidence.implementer,
        &mut evidence.reviewer,
        &mut evidence.pilot,
    ]
}

/// Every task id the pack names: the tasks behind each problem and their
/// regression fixes.
fn evidence_ids(evidence: &ModelEvidence) -> BTreeSet<String> {
    [&evidence.implementer, &evidence.reviewer, &evidence.pilot]
        .into_iter()
        .flat_map(|section| section.problems.iter())
        .flat_map(|problem| problem.tasks.iter())
        .flat_map(|task| {
            std::iter::once(task.task_id.clone())
                .chain(task.regression_fixes.iter().map(|fix| fix.task_id.clone()))
        })
        .collect()
}

fn section_value(section: &EvidenceSection) -> Value {
    json!({
        "window": { "since": section.since, "until": section.until },
        "problems": section.problems.iter().map(|problem| json!({
            "description": problem.description,
            "tasks": problem.tasks.iter().map(|task| json!({
                "task_id": task.task_id,
                "title": task.title,
                "occurrences": task.occurrences.iter().map(|occurrence| json!({
                    "at": occurrence.at.to_rfc3339_opts(SecondsFormat::Secs, true),
                    "detail": occurrence.detail,
                })).collect::<Vec<_>>(),
                "regression_fixes": task.regression_fixes.iter().map(|fix| json!({
                    "task_id": fix.task_id,
                    "title": fix.title,
                })).collect::<Vec<_>>(),
                "paths": task.paths,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn pack_value(
    key: &MemoryKey,
    week: &str,
    now: DateTime<Utc>,
    evidence: &ModelEvidence,
    evidence_ids: &BTreeSet<String>,
    dropped_tasks: usize,
    dropped_occurrences: usize,
) -> Value {
    let note = if dropped_tasks == 0 {
        "Nothing was dropped.".to_string()
    } else {
        format!(
            "The pack is capped at {CREW_MEMORY_EVIDENCE_MAX_BYTES} bytes: the {dropped_tasks} oldest task entries ({dropped_occurrences} occurrences) were dropped, keeping each problem's most recent occurrences."
        )
    };
    json!({
        "schema_version": PACK_SCHEMA_VERSION,
        "key": key,
        "week": week,
        "built_at": now.to_rfc3339_opts(SecondsFormat::Secs, true),
        "about": "Work this model did in this workspace that ran into a problem, pooled over the crews that run it: as an implementer and a pilot in the past 7 days, and as a reviewer in the settled 30 days ending 30 days ago. Each problem is described in plain words with the tasks behind it. Cite these task ids (evidence_ids) as the evidence for a note; no other ids are accepted.",
        "implementer": section_value(&evidence.implementer),
        "reviewer": section_value(&evidence.reviewer),
        "pilot": section_value(&evidence.pilot),
        "evidence_ids": evidence_ids,
        "dropped": {
            "tasks": dropped_tasks,
            "occurrences": dropped_occurrences,
            "max_bytes": CREW_MEMORY_EVIDENCE_MAX_BYTES,
            "note": note,
        },
    })
}
