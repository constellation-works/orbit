//! Precedent auto-answer, reversal, and the consolidation proposal.
//!
//! Auto-answer runs from an agent raise when one current rule the operator
//! marked `auto` matches the request's topic exactly. The answer is a
//! precedent record, not a human answer, and it does not append a corpus
//! outbox entry. Reversal re-opens that request, and re-opens a model answer
//! the same way (see `decision_model`). Consolidation returns
//! proposed markdown and writes no rule file.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_REOPENED_EVENT, DecisionGate, DecisionRequest,
    DecisionRequestState, DecisionResolution, MODEL_ANSWER_ACTOR, PRECEDENT_ACTOR,
    PrecedentCitation, PrecedentSource, TaskHistoryEntry, TaskStatus,
};
use serde_json::{Value, json};

use super::super::TaskRecordUpdateParams;
use super::super::params::TaskUpdateParams;
use super::rules::{
    ConsolidationAnswer, CorpusDecisionDocument, CorpusDigest, CorpusRuleDependency,
    consolidate_decision_corpus, corpus_config_digest, corpus_content_digest,
    match_auto_decision_rule, parse_decision_rules, set_rule_auto_in_markdown,
};
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;
use crate::application::task::base_revision::{list_blob_paths_at, read_blob_at};

const CORPUS_REL: &str = "docs/agents/corpus";

impl OrbitRuntime {
    /// Answer `request` from a matching operator-approved rule, or return it
    /// unchanged when nothing matches. A missing or unreadable corpus leaves
    /// the request open. The caller holds the task write lock.
    ///
    /// Rules and path dependency digests are read at the workspace's pinned
    /// base revision (see `workspace_base_revision`), the same revision
    /// corpus injection reads at, never from the live working tree: an
    /// uncommitted or unmerged rule cannot answer a request.
    pub(crate) fn auto_answer_from_precedent(
        &self,
        task_status: TaskStatus,
        request: &DecisionRequest,
    ) -> Result<DecisionRequest, OrbitError> {
        let Some(topic) = request.topic.as_deref() else {
            return Ok(request.clone());
        };
        if request.gate != DecisionGate::AgentRequest {
            return Ok(request.clone());
        }
        let Ok(revision) = self.workspace_base_revision() else {
            return Ok(request.clone());
        };
        let documents = self.read_corpus_decision_documents_at(&revision);
        let digests = self.corpus_digests_at_revision(&documents, &revision);
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let option_ids = request
            .answers
            .iter()
            .map(|answer| answer.id.clone())
            .collect::<Vec<_>>();
        let Some(rule) = match_auto_decision_rule(&documents, topic, &option_ids, &today, &digests)
        else {
            return Ok(request.clone());
        };
        let citations = rule
            .citations
            .iter()
            .map(|citation| {
                format!(
                    "{} on {} chose {}",
                    citation.task, citation.date, citation.option
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let citation_text = if citations.is_empty() {
            "none recorded".to_string()
        } else {
            citations
        };
        let comment = format!(
            "Precedent answer: rule `{}` chose option `{}` because the operator approved auto on this current rule. Citations: {citation_text}. Reverse it with `orbit decision reverse {}`, which re-opens the request.",
            rule.id, rule.answer, request.id
        );
        self.update_task_as_precedent(
            &request.task_id,
            TaskUpdateParams {
                comment: Some(comment),
                status: Some(TaskStatus::Done),
                ..Default::default()
            },
        )?;
        let now = chrono::Utc::now();
        let resolution = DecisionResolution {
            request_id: request.id.clone(),
            answer_id: rule.answer.clone(),
            answered_by: PRECEDENT_ACTOR.to_string(),
            provenance: Some("precedent".to_string()),
            note: Some(format!("operator approved auto on rule {}", rule.id)),
            answered_at: now,
            precedent: Some(PrecedentCitation {
                rule_id: rule.id,
                citations: rule
                    .citations
                    .into_iter()
                    .map(|citation| PrecedentSource {
                        task: citation.task,
                        date: citation.date,
                        option: citation.option,
                    })
                    .collect(),
                operator_approved: true,
                previous_status: Some(task_status),
            }),
            model: None,
            confirmation: None,
        };
        let entry = TaskHistoryEntry {
            at: now,
            by: PRECEDENT_ACTOR.to_string(),
            event: DECISION_ANSWERED_EVENT.to_string(),
            note: serde_json::to_string(&resolution).ok(),
            from_status: None,
            to_status: None,
            operation_id: Some(request.id.clone()),
            decision: None,
        };
        self.stores().task_records().update(
            &request.task_id,
            TaskRecordUpdateParams {
                actor: PRECEDENT_ACTOR.to_string(),
                append_history: vec![entry],
                ..Default::default()
            },
        )?;
        self.get_task_decision_requests(&request.task_id)?
            .into_iter()
            .find(|candidate| candidate.id == request.id)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "precedent answer for `{}` was not recorded",
                    request.id
                ))
            })
    }

    /// Re-open an automation answer (a `precedent` rule answer or a `model`
    /// answer) and restore the task status it recorded. Only an operator
    /// label may call this. Automation, `precedent` and `model` are refused,
    /// and a human answer is not reversible here.
    pub fn reverse_decision_answer(
        &self,
        request_id: &str,
        actor_label: &str,
    ) -> Result<DecisionRequest, OrbitError> {
        self.ensure_operator_precedent_actor(actor_label, "reverse an automation answer")?;
        let not_found = || {
            OrbitError::decision_refused(
                DecisionRefusalCode::RequestNotFound,
                format!("no decision request `{request_id}` in this workspace"),
            )
        };
        let located = self
            .get_decision_request(request_id)?
            .ok_or_else(not_found)?;
        let task_id = located.task_id.clone();
        let mut reopened = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let request = self
                    .get_task_decision_requests(&task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request_id)
                    .ok_or_else(not_found)?;
                self.reopen_automation_answer(&request, actor_label.trim())?;
                reopened = self
                    .get_task_decision_requests(&task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request_id);
                Ok(())
            })?;
        let reopened = reopened.ok_or_else(|| {
            OrbitError::Execution(format!(
                "decision request `{request_id}` vanished after it was re-opened"
            ))
        })?;
        Ok(reopened)
    }

    /// The reversal step of `orbit decision reverse`: check `request` holds a
    /// reversible automation answer (a precedent answer, or a model answer
    /// recorded before decision assistance was removed [ORB-22808]),
    /// restore the status it recorded and append the re-open event as
    /// `actor_label`. The caller holds the task write lock.
    pub(super) fn reopen_automation_answer(
        &self,
        request: &DecisionRequest,
        actor_label: &str,
    ) -> Result<(), OrbitError> {
        let request_id = request.id.as_str();
        let task_id = request.task_id.as_str();
        let refuse = |why: &str| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!("decision request `{request_id}` {why}; nothing was changed"),
            )
        };
        let Some(resolution) = request
            .resolution
            .as_ref()
            .filter(|_| request.state == DecisionRequestState::Answered)
        else {
            return Err(refuse("is not an openable automation answer"));
        };
        let (writer, previous, kind) = match resolution.answered_by.as_str() {
            PRECEDENT_ACTOR => {
                let Some(precedent) = resolution.precedent.as_ref() else {
                    return Err(refuse("has no precedent record to reverse"));
                };
                if !precedent.operator_approved {
                    return Err(refuse("was not approved by the operator"));
                }
                (PRECEDENT_ACTOR, precedent.previous_status, "Precedent")
            }
            MODEL_ANSWER_ACTOR => {
                let Some(model) = resolution.model.as_ref() else {
                    return Err(refuse("has no model record to reverse"));
                };
                (MODEL_ANSWER_ACTOR, model.previous_status, "Model answer")
            }
            _ => return Err(refuse("is not an openable automation answer")),
        };
        let Some(previous) = previous else {
            return Err(refuse("does not record the status to restore"));
        };
        let task = self.get_task(task_id)?;
        if task.status != previous {
            let comment =
                format!("{kind} reversed by {actor_label}: request {request_id} is open again.");
            self.update_task_as_decision_automation(
                task_id,
                TaskUpdateParams {
                    comment: Some(comment),
                    status: Some(previous),
                    ..Default::default()
                },
                writer,
            )?;
        }
        let entry = TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor_label.to_string(),
            event: DECISION_REOPENED_EVENT.to_string(),
            note: Some(json!({"request_id": request_id}).to_string()),
            from_status: None,
            to_status: None,
            operation_id: Some(request_id.to_string()),
            decision: None,
        };
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: actor_label.to_string(),
                append_history: vec![entry],
                ..Default::default()
            },
        )?;
        Ok(())
    }

    /// Set `auto` on one current rule. Only an operator label may call this.
    /// `auto_set_by` becomes `operator` when auto is turned on, and is cleared
    /// when auto is turned off. The edited file is the one-line curation edit
    /// the operator commits; consolidation never calls this.
    pub fn set_decision_rule_auto(
        &self,
        rule_ref: &str,
        auto: bool,
        actor_label: &str,
    ) -> Result<(), OrbitError> {
        self.ensure_operator_precedent_actor(actor_label, "set auto on a decision rule")?;
        let path = self.locate_rule_file(rule_ref)?;
        let markdown = fs::read_to_string(&path).map_err(|error| {
            OrbitError::InvalidInput(format!("could not read {}: {error}", path.display()))
        })?;
        let Some((rewritten, _)) = set_rule_auto_in_markdown(&markdown, rule_ref, auto) else {
            return Err(OrbitError::InvalidInput(format!(
                "no current rule `{rule_ref}` to edit"
            )));
        };
        if rewritten == markdown {
            return Ok(());
        }
        atomic_write_text(&path, &rewritten).map_err(|error| {
            OrbitError::Execution(format!("could not write {}: {error}", path.display()))
        })?;
        Ok(())
    }

    /// Propose corpus edits for human answers on or after `since` (`YYYY-MM-DD`).
    /// `since` omitted includes every human answer. The result is proposal
    /// text. This writes no corpus file.
    pub fn consolidate_decision_precedent(&self, since: Option<&str>) -> Result<Value, OrbitError> {
        let since = since
            .map(|value| {
                NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
                    OrbitError::InvalidInput("`since` must be a YYYY-MM-DD date".to_string())
                })
            })
            .transpose()?;
        let requests = self.list_decision_requests(Some(DecisionGate::AgentRequest), None, true)?;
        let mut answers = Vec::new();
        for request in requests {
            if request.state != DecisionRequestState::Answered {
                continue;
            }
            let Some(resolution) = request.resolution else {
                continue;
            };
            if resolution.is_automation_answer() {
                continue;
            }
            let answered_on = resolution.answered_at.date_naive();
            if since.is_some_and(|since| answered_on < since) {
                continue;
            }
            let Some(topic) = request.topic else {
                continue;
            };
            let Some(question) = request.summary else {
                continue;
            };
            answers.push(ConsolidationAnswer {
                task_id: request.task_id,
                topic,
                question,
                answer_id: resolution.answer_id,
                note: resolution.note,
                answered_on: answered_on.format("%Y-%m-%d").to_string(),
            });
        }
        let documents = self.read_corpus_decision_documents();
        let digests = self.corpus_digests_for(&documents);
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let proposals = consolidate_decision_corpus(&documents, &answers, &digests, &today);
        Ok(json!({
            "count": proposals.len(),
            "proposals": proposals
                .into_iter()
                .map(|proposal| json!({
                    "path": format!("{CORPUS_REL}/{}", proposal.path),
                    "markdown": proposal.markdown,
                }))
                .collect::<Vec<_>>(),
        }))
    }

    fn ensure_operator_precedent_actor(
        &self,
        actor_label: &str,
        action: &str,
    ) -> Result<(), OrbitError> {
        let actor = actor_label.trim();
        if actor.is_empty()
            || is_automation_actor(actor)
            || actor == PRECEDENT_ACTOR
            || actor == MODEL_ANSWER_ACTOR
        {
            return Err(OrbitError::PolicyDenied(format!(
                "only the operator can {action}; `{actor}` was refused and nothing was changed"
            )));
        }
        Ok(())
    }

    fn corpus_root(&self) -> PathBuf {
        self.paths().repo_root.join(CORPUS_REL)
    }

    fn read_corpus_decision_documents(&self) -> Vec<CorpusDecisionDocument> {
        let dir = self.corpus_root().join("decisions");
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut documents = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.ends_with(".md") {
                continue;
            }
            let Ok(markdown) = fs::read_to_string(entry.path()) else {
                continue;
            };
            documents.push(CorpusDecisionDocument {
                path: format!("decisions/{name}"),
                markdown,
            });
        }
        documents.sort_by(|left, right| left.path.cmp(&right.path));
        documents
    }

    /// Decision documents at `revision`, in the same shape the working-tree
    /// read returns. A corpus absent at that revision reads as empty.
    fn read_corpus_decision_documents_at(&self, revision: &str) -> Vec<CorpusDecisionDocument> {
        let dir = format!("{CORPUS_REL}/decisions");
        let Ok(paths) = list_blob_paths_at(&self.paths().repo_root, revision, &dir) else {
            return Vec::new();
        };
        let mut documents = Vec::new();
        for path in paths {
            let Some(name) = path.rsplit('/').next() else {
                continue;
            };
            if !name.ends_with(".md") {
                continue;
            }
            let Ok(bytes) = read_blob_at(&self.paths().repo_root, revision, &path) else {
                continue;
            };
            let Ok(markdown) = String::from_utf8(bytes) else {
                continue;
            };
            documents.push(CorpusDecisionDocument {
                path: format!("decisions/{name}"),
                markdown,
            });
        }
        documents.sort_by(|left, right| left.path.cmp(&right.path));
        documents
    }

    fn corpus_digests_at_revision(
        &self,
        documents: &[CorpusDecisionDocument],
        revision: &str,
    ) -> Vec<CorpusDigest> {
        let mut digests = Vec::new();
        for document in documents {
            for rule in parse_decision_rules(&document.markdown) {
                for dependency in rule.depends_on {
                    if digests.iter().any(|digest: &CorpusDigest| {
                        digest.kind == dependency.kind && digest.name == dependency.name
                    }) {
                        continue;
                    }
                    digests.push(self.corpus_digest_at_revision(revision, &dependency));
                }
            }
        }
        digests
    }

    fn corpus_digests_for(&self, documents: &[CorpusDecisionDocument]) -> Vec<CorpusDigest> {
        let mut digests = Vec::new();
        for document in documents {
            for rule in parse_decision_rules(&document.markdown) {
                for dependency in rule.depends_on {
                    if digests.iter().any(|digest: &CorpusDigest| {
                        digest.kind == dependency.kind && digest.name == dependency.name
                    }) {
                        continue;
                    }
                    digests.push(self.digest_for_dependency(&dependency));
                }
            }
        }
        digests
    }

    fn digest_for_dependency(&self, dependency: &CorpusRuleDependency) -> CorpusDigest {
        let digest = match dependency.kind.as_str() {
            "path" => {
                let path = self.paths().repo_root.join(&dependency.name);
                match fs::read(&path) {
                    Ok(bytes) if path_stays_in(&self.paths().repo_root, &path) => {
                        corpus_content_digest(&bytes)
                    }
                    _ => corpus_content_digest(b"missing"),
                }
            }
            "config" => {
                let text = fs::read_to_string(self.paths().orbit_dir.join("config.toml"))
                    .unwrap_or_default();
                let value = toml::from_str::<toml::Value>(&text)
                    .ok()
                    .and_then(|document| dotted_toml(&document, &dependency.name));
                corpus_config_digest(&dependency.name, value.as_deref())
            }
            _ => corpus_content_digest(b"missing"),
        };
        CorpusDigest {
            kind: dependency.kind.clone(),
            name: dependency.name.clone(),
            digest,
        }
    }

    fn locate_rule_file(&self, rule_ref: &str) -> Result<PathBuf, OrbitError> {
        let documents = self.read_corpus_decision_documents();
        let relative = if let Some((stem, rule_id)) = rule_ref.split_once('/') {
            if rule_ref.matches('/').count() != 1 {
                return Err(OrbitError::InvalidInput(
                    "a rule reference is `topic/rule` or a single rule id".into(),
                ));
            }
            let path = format!("decisions/{stem}.md");
            let holds = documents.iter().any(|document| {
                document.path == path
                    && parse_decision_rules(&document.markdown)
                        .iter()
                        .any(|rule| rule.id == rule_id)
            });
            if !holds {
                return Err(OrbitError::InvalidInput(format!(
                    "no rule `{rule_ref}` in the decision corpus"
                )));
            }
            path
        } else {
            let mut holders = Vec::new();
            for document in &documents {
                if parse_decision_rules(&document.markdown)
                    .iter()
                    .any(|rule| rule.id == rule_ref)
                {
                    holders.push(document.path.clone());
                }
            }
            match holders.as_slice() {
                [one] => one.clone(),
                [] => {
                    return Err(OrbitError::InvalidInput(format!(
                        "no rule `{rule_ref}` in the decision corpus"
                    )));
                }
                _ => {
                    return Err(OrbitError::InvalidInput(format!(
                        "rule id `{rule_ref}` is in more than one topic file; pass `topic/{rule_ref}`"
                    )));
                }
            }
        };
        Ok(self.corpus_root().join(relative))
    }
}

fn path_stays_in(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

fn dotted_toml(document: &toml::Value, key: &str) -> Option<String> {
    let mut current = document;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    Some(match current {
        toml::Value::String(value) => value.clone(),
        other => other.to_string(),
    })
}
