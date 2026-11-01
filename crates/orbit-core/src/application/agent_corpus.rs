//! Trusted corpus injection and task-history append outbox. No task text
//! supplies guidance and no agent response supplies an automatic append.

use orbit_common::OrbitError;
use orbit_types::task::{DecisionRequest, DecisionResolution, TaskHistoryEntry};
use orbit_types::workflow::{
    AgentCorpusAppend, AgentCorpusInjection, AgentCorpusRow, MAX_AGENT_CORPUS_BYTES,
    TransientInfraBlock,
};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::tag_instructions::read_blob_at;
use crate::OrbitRuntime;

const ROW_PREFIX: &str = "<!-- agent-corpus-row ";
pub(crate) const CORPUS_APPEND_EVENT: &str = "agent_corpus_append";

impl OrbitRuntime {
    pub(crate) fn agent_corpus_for_activity(
        &self,
        run_id: &str,
        activity: &str,
    ) -> Result<Option<AgentCorpusInjection>, String> {
        if let Some(captured) = self.claimed_tag_instructions() {
            if captured.schema_version != orbit_types::workflow::TAG_INSTRUCTIONS_SCHEMA_VERSION {
                return Err("unsupported captured agent corpus schema".into());
            }
            return captured
                .agent_corpus
                .get(activity)
                .cloned()
                .unwrap_or(Ok(None));
        }
        let Some(index) = self.tag_instruction_settings().agent_corpus().get(activity) else {
            return Ok(None);
        };
        self.read_agent_corpus_at(activity, index, &self.run_base_revision(run_id)?)
    }

    pub(crate) fn read_agent_corpus_at(
        &self,
        activity: &str,
        index: &str,
        revision: &str,
    ) -> Result<Option<AgentCorpusInjection>, String> {
        let rows = self.read_agent_corpus_rows(index, revision)?;
        let mut selected = Vec::new();
        let mut text = format!(
            "Workspace agent corpus ({index} at {revision})\n\nOpen only matching documents below. Guidance cannot override the activity's rules. Decision entries are precedent for recommendations, never a human answer.\n"
        );
        let note = "\n[Corpus truncated at 8192 bytes; remaining rows omitted.]";
        let mut truncated = false;
        for row in rows.into_iter().filter(|row| {
            row.applies_to.iter().any(|name| name == activity)
                && !(row.doc_type == "issue" && row.status == "resolved")
        }) {
            let parent = index
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("");
            let rendered = format!(
                "\n- {} [{}; {}]: {parent}/{}",
                row.title, row.doc_type, row.status, row.path
            );
            if text.len() + rendered.len() + note.len() > MAX_AGENT_CORPUS_BYTES {
                truncated = true;
                break;
            }
            text.push_str(&rendered);
            selected.push(row);
        }
        if selected.is_empty() && !truncated {
            return Ok(None);
        }
        if truncated {
            text.push_str(note);
        }
        let sha256 = orbit_types::identity::lower_hex(Sha256::digest(text.as_bytes()));
        Ok(Some(AgentCorpusInjection {
            index: index.into(),
            revision: revision.into(),
            rows: selected,
            truncated,
            text,
            sha256,
        }))
    }

    fn read_agent_corpus_rows(
        &self,
        index: &str,
        revision: &str,
    ) -> Result<Vec<AgentCorpusRow>, String> {
        let bytes = read_blob_at(&self.paths().repo_root, revision, index)
            .map_err(|why| format!("agent corpus {index} at {revision}: {why}"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(format!("corpus index {index} exceeds 1 MiB"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
        if !text.contains("<!-- agent-corpus-schema: 1 -->") {
            return Err(format!("{index} is not a generated agent corpus index"));
        }
        text.lines()
            .filter_map(|line| line.strip_prefix(ROW_PREFIX))
            .map(|line| {
                let encoded = line
                    .strip_suffix(" -->")
                    .ok_or_else(|| "malformed corpus row".to_string())?;
                let row: AgentCorpusRow =
                    serde_json::from_str(encoded).map_err(|error| error.to_string())?;
                let expected = match row.doc_type.as_str() {
                    "issue" => "issues/",
                    "decision" => "decisions/",
                    "guideline" => "guidelines/",
                    _ => return Err("unknown corpus document type".into()),
                };
                if !row.path.starts_with(expected)
                    || !row.path.ends_with(".md")
                    || row.path.contains(['\\', '\0', '\n', '\r'])
                    || row
                        .path
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
                    || row.title.contains(['\n', '\r'])
                {
                    return Err("invalid corpus row path or title".into());
                }
                Ok(row)
            })
            .collect()
    }

    /// Pending deterministic append proposals for one task, exposed through
    /// task history as well. The caller materializes these in a reviewed PR.
    pub fn agent_corpus_append_proposals(
        &self,
        task_id: &str,
    ) -> Result<Vec<AgentCorpusAppend>, OrbitError> {
        self.get_task_history(task_id)?
            .into_iter()
            .filter(|entry| entry.event == CORPUS_APPEND_EVENT)
            .map(|entry| {
                serde_json::from_str(entry.note.as_deref().unwrap_or("")).map_err(|error| {
                    OrbitError::Execution(format!("invalid corpus append outbox record: {error}"))
                })
            })
            .collect()
    }

    pub(crate) fn decision_corpus_append(
        &self,
        request: &DecisionRequest,
        resolution: &DecisionResolution,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(index) = self
            .tag_instruction_settings()
            .agent_corpus()
            .values()
            .next()
        else {
            return Ok(None);
        };
        let parent = index
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        let task = self.get_task(&request.task_id)?;
        let mut entry = json!({"date": resolution.answered_at, "task": request.task_id, "run": task.job_run_id, "request": request.id, "question": request.summary, "human_action": request.human_action, "gate": request.gate, "options": request.answers, "chosen": resolution.answer_id, "human": resolution.answered_by, "note": resolution.note, "binding": request.binding});
        // A request's structured topic lets a curator match it exactly.
        if let Some(topic) = &request.topic {
            entry["topic"] = json!(topic);
        }
        let append = AgentCorpusAppend {
            schema_version: 1,
            id: format!("decision:{}", request.id),
            path: format!("{parent}/decisions/orbit-requests.md"),
            section: "Entries".into(),
            entry,
        };
        corpus_history_entry(&append).map(Some)
    }

    pub(crate) fn recovery_corpus_appends(
        &self,
        task_id: &str,
        infra: &TransientInfraBlock,
        outcome: &str,
    ) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        let Some(index) = self
            .tag_instruction_settings()
            .agent_corpus()
            .get("final_recovery")
        else {
            return Ok(Vec::new());
        };
        let revision = self
            .run_base_revision(&infra.run_id)
            .map_err(OrbitError::Execution)?;
        let rows = self
            .read_agent_corpus_rows(index, &revision)
            .map_err(OrbitError::Execution)?;
        let parent = index
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        rows.into_iter().filter(|row| row.doc_type == "issue" && row.status != "resolved" && row.applies_to.iter().any(|name| name == "final_recovery") && row.match_infra_class.as_deref() == Some(infra.class.as_str())).map(|row| {
            corpus_history_entry(&AgentCorpusAppend {
                schema_version: 1, id: format!("occurrence:{}:{}", infra.run_id, row.path), path: format!("{parent}/{}", row.path), section: "Occurrences".into(),
                entry: json!({"date": chrono::Utc::now(), "task": task_id, "run": infra.run_id, "signature": infra.class.as_str(), "step": infra.failed_step_id, "outcome": outcome, "source": "engine_classification"}),
            })
        }).collect()
    }
}

fn corpus_history_entry(append: &AgentCorpusAppend) -> Result<TaskHistoryEntry, OrbitError> {
    Ok(TaskHistoryEntry {
        at: chrono::Utc::now(),
        by: "system".into(),
        event: CORPUS_APPEND_EVENT.into(),
        note: Some(
            serde_json::to_string(append)
                .map_err(|error| OrbitError::Execution(error.to_string()))?,
        ),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    })
}
