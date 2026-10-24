//! The one task-write response projection every surface serves.
//!
//! A write (`add`, `update`, `artifact.put`, and the dashboard's
//! approve/reject/comment mutations) answers a task record plus the
//! metadata the write itself established. Which record, and which
//! extras, is a policy this module owns; a surface only parses its
//! input, selects its existing profile, and renders the result
//! (the CLI's stderr warnings, the remote route's `host` stamp).
//!
//! The profiles preserve the contracts each surface already had:
//!
//! - [`TaskWriteResponsePolicy::FullRecord`] — [`task_record_json`]:
//!   sidecars, `review` and `pilot_warnings`. The local CLI and the
//!   HTTP mutations serve it.
//! - [`TaskWriteResponsePolicy::DashboardFullRecord`] — the full
//!   record plus the dashboard extras (linked pull requests,
//!   `status_transitions`, `job_run_navigable`,
//!   `readiness.preparing`). Only HTTP serves it.
//! - [`TaskWriteResponsePolicy::Compact`] — [`task_base_json`] plus
//!   OS and crew enrichments, no sidecar hydration. The tool host
//!   (MCP) and therefore a routed remote CLI answer serve it.
//! - [`TaskWriteResponsePolicy::Projected`] — the caller's explicit
//!   `fields`/`field` projection, a bare value for one field. Only
//!   the tool host selects it; the CLI rehydrates the full record
//!   and HTTP has no selector.
//!
//! Writes never carry the show-only `block`, `next_step`,
//! `final_recovery` or `owed_criteria` guidance: those stay on
//! [`super::task_show_json`].

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus, is_valid_orb_task_id, task_base_json};
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

use super::lifecycle::{task_status_transition_allowed, task_status_transition_required_field};
use super::pull_request_links::PullRequestLinks;
use super::record_json::{task_record_json, task_show_field_json};
use super::screened::ScreenedTaskWrite;
use super::{TaskOsReadout, TaskRow};

/// Which existing write profile a surface serves. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskWriteResponsePolicy {
    /// [`task_record_json`] with sidecars.
    FullRecord,
    /// The full record plus the dashboard extras.
    DashboardFullRecord,
    /// Base record plus OS/crew enrichments, no sidecars.
    Compact,
    /// Only the named show fields; one field answers its bare value.
    Projected(Vec<String>),
}

impl TaskWriteResponsePolicy {
    /// The tool host's policy for a write: the requested projection
    /// when `fields` names any, otherwise [`Self::Compact`].
    pub fn from_fields(fields: Option<&[String]>) -> Self {
        match fields {
            Some(fields) => Self::Projected(fields.to_vec()),
            None => Self::Compact,
        }
    }
}

/// What a write established beside the stored task: advisory
/// `warnings`, the `context_files_unverified` selectors a relaxed
/// owning-run write stored, and the optional redaction report a
/// tool answer carried.
///
/// Each field is inserted only under its existing rule — warnings
/// and unverified selectors only when non-empty, the redaction
/// report only when one was applied — so a strict write's response
/// is the bare record. The projection never invents a report:
/// a surface that has none (HTTP) leaves it unset, and persistence
/// redaction and its audit are untouched.
#[derive(Debug, Clone, Default)]
pub struct TaskWriteMetadata {
    /// Advisory signals about the stored task.
    pub warnings: Vec<String>,
    /// Selectors stored without a verified target.
    pub context_files_unverified: Vec<String>,
    /// The `redactions` value to carry with `redactions_applied`.
    pub redactions: Option<Value>,
}

impl TaskWriteMetadata {
    /// The metadata a screened write established.
    pub fn from_screened(written: &ScreenedTaskWrite) -> Self {
        Self {
            warnings: written.warnings.clone(),
            context_files_unverified: written.context_files_unverified.clone(),
            redactions: None,
        }
    }

    /// The metadata a tool answer carries, for a surface (the local
    /// CLI) that re-renders a tool write in its own profile.
    pub fn from_tool_answer(answer: &Value) -> Self {
        let strings = |key: &str| {
            answer
                .get(key)
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        Self {
            warnings: strings("warnings"),
            context_files_unverified: strings("context_files_unverified"),
            redactions: (answer.get("redactions_applied") == Some(&Value::Bool(true)))
                .then(|| answer.get("redactions").cloned().unwrap_or(Value::Null)),
        }
    }

    /// Insert this metadata into a response under the nonempty rule.
    /// A non-object response (a bare single-field projection) is
    /// left as it is, as the tool host always left it.
    pub fn insert_into(&self, response: &mut Value) {
        let Some(object) = response.as_object_mut() else {
            return;
        };
        if !self.context_files_unverified.is_empty() {
            object.insert(
                "context_files_unverified".to_string(),
                json!(self.context_files_unverified),
            );
        }
        if !self.warnings.is_empty() {
            object.insert("warnings".to_string(), json!(self.warnings));
        }
        if let Some(redactions) = &self.redactions {
            object.insert("redactions_applied".to_string(), Value::Bool(true));
            object.insert("redactions".to_string(), redactions.clone());
        }
    }
}

/// Build a task write's response: `task` projected under `policy`,
/// with `metadata` inserted under its rules.
pub fn task_write_response_json(
    runtime: &OrbitRuntime,
    task: &Task,
    policy: &TaskWriteResponsePolicy,
    metadata: &TaskWriteMetadata,
) -> Result<Value, OrbitError> {
    let mut response = match policy {
        TaskWriteResponsePolicy::FullRecord | TaskWriteResponsePolicy::DashboardFullRecord => {
            let status_by_id = runtime.dependency_status_index([task])?;
            let row = runtime.task_row_with_sidecars(task.clone())?;
            let object = task_record_json(runtime, &row, &status_by_id)?;
            if *policy == TaskWriteResponsePolicy::DashboardFullRecord {
                with_dashboard_extras(runtime, &row.task, object)?
            } else {
                Value::Object(object)
            }
        }
        TaskWriteResponsePolicy::Compact => {
            let status_by_id = task_reference_status_index(runtime, task)?;
            let mut object = task_base_json(task, &status_by_id);
            TaskOsReadout::new(runtime)?.insert_into(task, &mut object);
            runtime.task_crew_read(task).insert_into(&mut object);
            Value::Object(object)
        }
        TaskWriteResponsePolicy::Projected(fields) => {
            task_fields_response_json(runtime, task, fields)?
        }
    };
    metadata.insert_into(&mut response);
    Ok(response)
}

/// A screened write's response under `policy`, with the write's own
/// warnings and unverified selectors as its metadata.
pub fn screened_task_write_response_json(
    runtime: &OrbitRuntime,
    written: &ScreenedTaskWrite,
    policy: &TaskWriteResponsePolicy,
) -> Result<Value, OrbitError> {
    task_write_response_json(
        runtime,
        &written.task,
        policy,
        &TaskWriteMetadata::from_screened(written),
    )
}

/// A tool write answer re-rendered in the full-record profile: the
/// stored task the answer names, re-read, plus the answer's
/// warnings, unverified selectors and redaction report. Returns
/// the metadata beside the document so the caller can render it
/// for a human (the CLI's stderr warnings).
pub fn task_write_response_from_tool_answer(
    runtime: &OrbitRuntime,
    answer: &Value,
) -> Result<(Value, Task, TaskWriteMetadata), OrbitError> {
    let id = answer.get("id").and_then(Value::as_str).ok_or_else(|| {
        OrbitError::Execution("task write answer did not name the task".to_string())
    })?;
    let task = runtime.get_task(id)?;
    let metadata = TaskWriteMetadata::from_tool_answer(answer);
    let document = task_write_response_json(
        runtime,
        &task,
        &TaskWriteResponsePolicy::FullRecord,
        &metadata,
    )?;
    Ok((document, task, metadata))
}

/// One task reduced to the requested show fields: the bare value
/// for a single field, otherwise an object keyed by field name.
pub fn task_fields_response_json(
    runtime: &OrbitRuntime,
    task: &Task,
    fields: &[String],
) -> Result<Value, OrbitError> {
    let status_by_id = if fields
        .iter()
        .any(|field| matches!(field.as_str(), "resolved_dependencies" | "relations"))
    {
        Some(task_reference_status_index(runtime, task)?)
    } else {
        None
    };
    if fields.len() == 1 {
        return task_show_field_json(runtime, task, &fields[0], status_by_id.as_ref());
    }
    let mut object = Map::new();
    for field in fields {
        object.insert(
            field.clone(),
            task_show_field_json(runtime, task, field, status_by_id.as_ref())?,
        );
    }
    Ok(Value::Object(object))
}

/// Resolve only the referenced task IDs needed by one task projection.
///
/// The list surface already owns a bounded status page and passes its index to
/// [`task_base_json`]. Individual task reads use the same registry projection,
/// bounded to the handful of dependency and relation targets, so they resolve
/// cross-workspace references without hydrating their bundles.
///
/// Relation targets are not always task ids — a `resolves` relation may
/// legitimately point at a friction, ADR, or learning id (e.g.
/// `F2026-05-001`). Only ids that parse as task ids are point-read; other
/// namespaces are skipped rather than sent through `get_task_row`, which
/// rejects them as invalid input instead of reporting them not found.
pub(crate) fn task_reference_status_index(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
    let referenced_ids = task
        .dependencies()
        .into_iter()
        .chain(
            task.relations
                .iter()
                .map(|relation| relation.target.clone()),
        )
        .filter(|id| is_valid_orb_task_id(id))
        .collect::<BTreeSet<_>>();
    let mut status_by_id = runtime.task_status_index_for(&referenced_ids)?;
    runtime.satisfy_completed_archived_dependencies(&mut status_by_id, [task]);
    Ok(status_by_id)
}

/// Linked pull requests, lifecycle transitions, run navigability and
/// `readiness.preparing`. These are dashboard controls, not part of
/// the shared task record; the dashboard detail and its write
/// responses add them, no other profile does.
pub fn with_dashboard_extras(
    runtime: &OrbitRuntime,
    task: &Task,
    mut object: Map<String, Value>,
) -> Result<Value, OrbitError> {
    link_pull_requests(&mut object, task, &PullRequestLinks::new(runtime));
    object.insert(
        "status_transitions".to_string(),
        dashboard_status_transitions(runtime, task)?,
    );
    // ORB-12516: whether `#runs?run_id=` can resolve this task's run *here*.
    // A run recorded against another machine lives in that host's job store, so
    // the detail names the host to inspect instead of linking to nothing. An
    // unrecorded host keeps the historical local link — the provenance line
    // still reads *unknown*, which is what the absent field means.
    object.insert(
        "job_run_navigable".to_string(),
        Value::Bool(job_run_is_locally_navigable(
            runtime.automation_machine_identity(),
            task,
        )),
    );
    let mut value = Value::Object(object);
    if value.get("readiness").is_some() {
        let preparing = tasks_in_pilot_preparation(runtime).contains(&task.id);
        mark_readiness_preparing(&mut value, preparing);
    }
    Ok(value)
}

/// Fill the page of a `github-pr` ref recorded without one, so the
/// dashboard can link every delivered pull request.
pub fn link_pull_requests(object: &mut Map<String, Value>, task: &Task, links: &PullRequestLinks) {
    let mut external_refs = task.external_refs.clone();
    links.link(&mut external_refs);
    object.insert("external_refs".to_string(), json!(external_refs));
}

/// Whether `#runs?run_id=` can resolve this task's run on the
/// machine `local_machine_id` names.
pub fn job_run_is_locally_navigable(local_machine_id: Option<&str>, task: &Task) -> bool {
    if task.job_run_id.is_none() {
        return false;
    }
    let Some(host) = task.job_run_machine.as_ref() else {
        return true;
    };
    // A recorded host has to *match* to navigate: with no local identity there
    // is nothing to prove the run is here, and an owner-local link would open
    // the wrong thing or nothing. An unrecorded host is handled above.
    local_machine_id.is_some_and(|local| local == host.machine_id)
}

/// Tasks an active task-pilot run is preparing. Their readiness gaps are about
/// to be settled by that run, so the dashboard shows them as preparing. The
/// flag is decoration on a read: a job store that cannot be read leaves every
/// task unmarked rather than failing the task read.
pub fn tasks_in_pilot_preparation(runtime: &OrbitRuntime) -> BTreeSet<String> {
    runtime
        .pilot_preparation_holders()
        .map(|holders| holders.into_keys().collect())
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "task readiness: active pilot preparations unreadable");
            BTreeSet::new()
        })
}

/// Add `readiness.preparing` to a projection that carries readiness.
pub fn mark_readiness_preparing(value: &mut Value, preparing: bool) {
    if let Some(readiness) = value.get_mut("readiness").and_then(Value::as_object_mut) {
        readiness.insert("preparing".to_string(), Value::Bool(preparing));
    }
}

const STATUS_ORDER: [TaskStatus; 9] = [
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Blocked,
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Someday,
    TaskStatus::Done,
    TaskStatus::Rejected,
    TaskStatus::Archived,
];

fn governed_status_targets(task: &Task) -> impl Iterator<Item = TaskStatus> {
    let current = task.status;
    STATUS_ORDER.into_iter().filter(move |target| {
        *target != current && task_status_transition_allowed(current, *target)
    })
}

fn dashboard_status_transitions(runtime: &OrbitRuntime, task: &Task) -> Result<Value, OrbitError> {
    governed_status_targets(task)
        .map(|target| {
            Ok(json!({
                "status": target.to_string(),
                "required_field": task_status_transition_required_field(runtime, task, target)?,
            }))
        })
        .collect::<Result<Vec<_>, OrbitError>>()
        .map(Value::Array)
}

/// The governed targets alone, for a summary row. `required_field` is deliberately absent rather
/// than `null`: the `done` requirement depends on the task's job run, which the
/// list path no longer reads, so a client that needs the requirement asks the
/// detail endpoint instead of treating "unknown" as "none".
pub fn summary_status_transitions(task: &Task) -> Value {
    Value::Array(
        governed_status_targets(task)
            .map(|target| json!({ "status": target.to_string() }))
            .collect(),
    )
}

/// The dashboard detail record for `row`: the show record plus the
/// dashboard extras. Kept beside the write projection so the detail
/// and a mutation response enrich a record the same way.
pub fn task_detail_with_dashboard_extras(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    status_by_id: &BTreeMap<String, TaskStatus>,
) -> Result<Value, OrbitError> {
    let object = super::record_json::task_show_json(runtime, row, status_by_id)?;
    with_dashboard_extras(runtime, &row.task, object)
}
