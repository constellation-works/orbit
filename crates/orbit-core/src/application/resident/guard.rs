//! The resident cycle's tool boundary: what a cycle's agent may call, and
//! the action ledger that makes a recovery cycle skip what its dead
//! predecessor already did.
//!
//! A call is a resident cycle call when the trusted run binding (never tool
//! input) names a `resident_cycle_pipeline` run. Reads always pass. Every
//! write needs the run to hold its mandate's live cycle and a recorded grant;
//! a right-gated write needs the right in that grant (observe-only grants
//! hold none), and any `workspace` the call names must be the assignment's
//! own workspace or one the grant covers. Moving a task to `done` also needs
//! its recorded pull requests closed: `orbit.workflow.land` is how a cycle lands
//! an open one. Each admitted write gets the next
//! `resident:<mandate>:<cycle>:<n>` key and a `started` row before it runs,
//! then one result row. In a recovery cycle, a call identical (tool and
//! canonical input) to one the dead cycle completed returns `already_done`
//! without running, and one the dead cycle left in doubt is refused.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    RESIDENT_ACTION_EVENT, RESIDENT_CYCLE_JOB, ResidentActionPhase, ResidentActionRow,
    ResidentCycleGrant, ResidentRight,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::cycle::live_cycle_from_history;
use super::due::cycle_grant;
use crate::OrbitRuntime;
use crate::runtime::workspace::catalog::WorkspaceScope;

/// What a tool call needs from a resident cycle's grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResidentCallClass {
    /// Changes nothing.
    Read,
    /// A write every cycle may make (file a proposed task, comment, raise a
    /// decision, post a notice, write its checkpoint).
    Open,
    /// A write that needs this right.
    Gated(ResidentRight),
    /// Moving a task to `done`: needs the `merge` right and no open pull
    /// request recorded for the task.
    CompleteTask,
    /// Moving a task to `review`: refused, naming the route that does it.
    Handoff,
    /// A write no resident cycle may make.
    Refused,
}

/// Where an admitted call stands.
#[derive(Debug)]
pub(crate) enum ResidentCallAdmission {
    /// Not a resident cycle call, or a read: run it as usual.
    Proceed,
    /// A ledgered write: run it, then report the result with this ticket.
    Ledgered(ResidentActionTicket),
    /// The dead cycle already did this: answer without running it.
    AlreadyDone(Value),
}

/// One admitted write's ledger entry.
#[derive(Debug)]
pub(crate) struct ResidentActionTicket {
    task_id: String,
    row: ResidentActionRow,
}

/// Every action row recorded under `cycle`, latest state per key, in key order.
pub(crate) fn action_rows(history: &[TaskHistoryEntry], cycle: &str) -> Vec<ResidentActionRow> {
    let mut rows: Vec<ResidentActionRow> = Vec::new();
    for row in history
        .iter()
        .filter(|entry| entry.event == RESIDENT_ACTION_EVENT)
        .filter_map(|entry| serde_json::from_str::<ResidentActionRow>(entry.note.as_deref()?).ok())
        .filter(|row| row.cycle == cycle)
    {
        match rows.iter_mut().find(|existing| existing.key == row.key) {
            Some(existing) => *existing = row,
            None => rows.push(row),
        }
    }
    rows
}

/// Recursively sort object keys; array order stays significant.
fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_value(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        other => other.clone(),
    }
}

fn digest_of(bytes: &[u8]) -> String {
    orbit_types::identity::lower_hex(Sha256::digest(bytes))
}

/// `sha256:v2:` over the call's canonical input (object keys sorted
/// recursively, array order preserved).
fn input_digest(tool: &str, input: &Value) -> String {
    let canonical = serde_json::to_vec(&json!([tool, canonical_value(input)])).unwrap_or_default();
    format!("sha256:v2:{}", digest_of(&canonical))
}

/// Pre-canonicalisation digest (`sha256:<hex>` over the input as
/// serialised). Only used to recognise byte-identical legacy rows.
fn legacy_input_digest(tool: &str, input: &Value) -> String {
    let bytes = serde_json::to_vec(&json!([tool, input])).unwrap_or_default();
    format!("sha256:{}", digest_of(&bytes))
}

fn is_legacy_digest(digest: &str) -> bool {
    digest.starts_with("sha256:") && !digest.starts_with("sha256:v2:")
}

fn subject_of(input: &Value) -> Option<String> {
    ["id", "task_id"]
        .into_iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .map(str::to_string)
}

/// The workspaces a call's input names.
fn named_workspaces(input: &Value) -> Vec<String> {
    let mut named = Vec::new();
    for key in ["workspace", "workspaces"] {
        match input.get(key) {
            Some(Value::String(value)) if !value.trim().is_empty() => {
                named.push(value.trim().to_string());
            }
            Some(Value::Array(values)) => named.extend(
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    named
}

/// The checkpoint tool's name.
const RESIDENT_CHECKPOINT_TOOL: &str = "orbit.resident.checkpoint";

/// The refusal of a resident's move to `done` while the task's PR is open.
const RESIDENT_DONE_PR_OPEN: &str = "resident_done_pr_open";

fn refused(code: &str, detail: impl std::fmt::Display) -> OrbitError {
    OrbitError::PolicyDenied(format!("{code}: {detail}"))
}

/// Refuse a right-gated write the grant does not hold the right for.
fn require_right(
    grant: &ResidentCycleGrant,
    tool: &str,
    right: ResidentRight,
) -> Result<(), OrbitError> {
    if grant.rights.contains(&right) {
        return Ok(());
    }
    let why = grant
        .narrowed_by
        .as_deref()
        .map(|cause| format!(" (observe-only: {cause})"))
        .unwrap_or_default();
    Err(refused(
        "resident_right_refused",
        format!("`{tool}` needs the `{}` right{why}", right.as_str()),
    ))
}

impl OrbitRuntime {
    /// Whether `run_id` is a resident cycle run homed in another workspace
    /// on this host: a call routed here by its `workspace` selector.
    fn foreign_resident_cycle_run(&self, run_id: &str) -> bool {
        let Some(catalog) = self.workspace_catalog() else {
            return false;
        };
        let (Ok(store), Ok(home)) = (self.sqlite_store(), self.workspace_id()) else {
            return false;
        };
        catalog
            .resolve_scope(&WorkspaceScope::AllRegistered)
            .unwrap_or_default()
            .into_iter()
            .filter(|target| target.workspace_id != home)
            .any(|target| {
                orbit_store::compose::workspace_job_run_store(store.clone(), target.workspace_id)
                    .get_job_run(run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|run| run.job_id == RESIDENT_CYCLE_JOB)
            })
    }

    /// The ids a selector names: this workspace's id for its own id or
    /// label, otherwise what the catalog resolves it to (or itself).
    fn resident_workspace_ids(&self, selector: &str) -> BTreeSet<String> {
        let mut ids = BTreeSet::from([selector.to_string()]);
        if let Ok(home) = self.workspace_id()
            && (selector == home || selector == self.workspace_label())
        {
            ids.insert(home);
            return ids;
        }
        if let Some(catalog) = self.workspace_catalog()
            && let Ok(targets) =
                catalog.resolve_scope(&WorkspaceScope::Selectors(vec![selector.to_string()]))
        {
            for target in targets {
                ids.insert(target.workspace_id);
                ids.insert(target.name);
            }
        }
        ids
    }

    /// Admit one tool call made under the trusted `run_id`, before it runs.
    pub(crate) fn admit_resident_cycle_call(
        &self,
        run_id: Option<&str>,
        tool: &str,
        class: ResidentCallClass,
        input: &Value,
    ) -> Result<ResidentCallAdmission, OrbitError> {
        let Some(run_id) = run_id.map(str::trim).filter(|id| !id.is_empty()) else {
            return Ok(ResidentCallAdmission::Proceed);
        };
        let run = match self.show_job_run_observed(run_id) {
            Ok(run) => run,
            Err(error) => {
                // Only a run that does not exist is not a resident cycle.
                // Any other lookup failure leaves the call unverified, so
                // a write fails closed instead of bypassing the guard.
                if matches!(
                    error,
                    OrbitError::NotFound {
                        kind: NotFoundKind::JobRun,
                        ..
                    }
                ) {
                    if class != ResidentCallClass::Read && self.foreign_resident_cycle_run(run_id) {
                        return Err(refused(
                            "resident_scope_refused",
                            "a resident cycle writes only through its mandate's own workspace",
                        ));
                    }
                    return Ok(ResidentCallAdmission::Proceed);
                }
                if class == ResidentCallClass::Read {
                    return Ok(ResidentCallAdmission::Proceed);
                }
                return Err(refused(
                    "resident_lookup_refused",
                    format!("could not verify the resident cycle run `{run_id}`: {error}"),
                ));
            }
        };
        if run.job_id != RESIDENT_CYCLE_JOB || class == ResidentCallClass::Read {
            return Ok(ResidentCallAdmission::Proceed);
        }
        let task_id = self.resident_cycle_mandate_of_run(run_id)?;
        let history = self.get_task_history(&task_id)?;
        let grant = live_cycle_from_history(&history)?
            .filter(|live| live.run_id == run_id && live.expires_at > Utc::now())
            .and_then(|live| cycle_grant(&history, &live.cycle))
            .ok_or_else(|| {
                refused(
                    "resident_cycle_stale",
                    "this run holds no live resident cycle; it may only read",
                )
            })?;
        self.check_resident_scope(&grant, input)?;
        match class {
            ResidentCallClass::Read | ResidentCallClass::Open => {}
            ResidentCallClass::Handoff => {
                return Err(refused(
                    "resident_handoff_by_checkpoint",
                    format!(
                        "a cycle does not move tasks to review; checkpoint with outcome `handoff` and finalize moves assignment {task_id}"
                    ),
                ));
            }
            ResidentCallClass::Gated(right) => require_right(&grant, tool, right)?,
            ResidentCallClass::CompleteTask => {
                require_right(&grant, tool, ResidentRight::Merge)?;
                self.refuse_done_with_open_pull_request(input)?;
            }
            ResidentCallClass::Refused => {
                return Err(refused(
                    "resident_tool_refused",
                    format!("a resident cycle may not call `{tool}`"),
                ));
            }
        }
        self.open_resident_action(&task_id, &grant, tool, input, &history)
    }

    /// A task whose delivery has not landed is not done: refuse the move while
    /// a pull request recorded for it is open. A forge that cannot answer
    /// refuses too. An unknown task is the update's own error to report.
    fn refuse_done_with_open_pull_request(&self, input: &Value) -> Result<(), OrbitError> {
        let Some(id) = subject_of(input) else {
            return Ok(());
        };
        let task = match self.get_task(&id) {
            Ok(task) => task,
            Err(OrbitError::NotFound { .. }) => return Ok(()),
            Err(error) => return Err(error),
        };
        let open = self.open_recorded_pull_requests(&task).map_err(|error| {
            refused(
                "resident_lookup_refused",
                format!("could not verify that task `{id}` has no open pull request: {error}"),
            )
        })?;
        match open.as_slice() {
            [] => Ok(()),
            open => Err(refused(
                RESIDENT_DONE_PR_OPEN,
                format!(
                    "task `{id}` still has open pull requests {open:?}; land it with \
                     `orbit.workflow` action `land` instead of marking it done"
                ),
            )),
        }
    }

    /// Every workspace the call names must be the assignment's own or one
    /// the grant covers.
    fn check_resident_scope(
        &self,
        grant: &ResidentCycleGrant,
        input: &Value,
    ) -> Result<(), OrbitError> {
        let mut allowed: BTreeSet<String> = grant.workspaces.iter().cloned().collect();
        allowed.insert(self.workspace_id()?);
        allowed.insert(self.workspace_label());
        for selector in named_workspaces(input) {
            if self.resident_workspace_ids(&selector).is_disjoint(&allowed) {
                return Err(refused(
                    "resident_scope_refused",
                    format!("workspace `{selector}` is outside the resident mandate's authority"),
                ));
            }
        }
        Ok(())
    }

    /// Key and record a write before it runs, or answer it from the dead
    /// cycle's ledger.
    fn open_resident_action(
        &self,
        task_id: &str,
        grant: &ResidentCycleGrant,
        tool: &str,
        input: &Value,
        history: &[TaskHistoryEntry],
    ) -> Result<ResidentCallAdmission, OrbitError> {
        let digest = input_digest(tool, input);
        if let Some(dead) = grant.recovery_of.as_deref() {
            let legacy_digest = legacy_input_digest(tool, input);
            let matches_row = |row: &ResidentActionRow| {
                row.input_digest == digest || row.input_digest == legacy_digest
            };
            let prior = action_rows(history, dead);
            if let Some(done) = prior.iter().find(|row| {
                matches_row(row)
                    && matches!(
                        row.phase,
                        ResidentActionPhase::Ok | ResidentActionPhase::AlreadyDone
                    )
            }) {
                return Ok(ResidentCallAdmission::AlreadyDone(json!({
                    "resident_action": "already_done",
                    "key": done.key,
                    "ref": done.reference,
                    "subject": done.subject,
                })));
            }
            if let Some(doubt) = prior
                .iter()
                .find(|row| matches_row(row) && row.phase == ResidentActionPhase::Started)
            {
                return Err(refused(
                    "resident_action_in_doubt",
                    format!(
                        "the dead cycle started this call as `{}` and never reported it; check its effect instead of repeating it",
                        doubt.key
                    ),
                ));
            }
            // A legacy row stores only the old order-sensitive digest, so a
            // non-matching call to the same tool cannot be told apart from a
            // reordered repeat of that row. Refuse rather than replay a
            // completed or in-doubt legacy mutation.
            if let Some(legacy) = prior.iter().find(|row| {
                is_legacy_digest(&row.input_digest)
                    && row.tool == tool
                    && matches!(
                        row.phase,
                        ResidentActionPhase::Ok
                            | ResidentActionPhase::AlreadyDone
                            | ResidentActionPhase::Started
                    )
            }) {
                return Err(refused(
                    "resident_action_in_doubt",
                    format!(
                        "the dead cycle recorded this tool as `{}` under a legacy input digest; its effect cannot be matched to this call, so check its effect instead of repeating it",
                        legacy.key
                    ),
                ));
            }
        }
        // The checkpoint is the cycle's record of its actions, not one of them.
        if tool == RESIDENT_CHECKPOINT_TOOL {
            return Ok(ResidentCallAdmission::Proceed);
        }
        let mut row = ResidentActionRow {
            key: String::new(),
            cycle: grant.cycle.clone(),
            tool: tool.to_string(),
            input_digest: digest,
            subject: subject_of(input),
            phase: ResidentActionPhase::Started,
            reference: None,
        };
        // Number under the assignment lock so concurrent calls get distinct keys.
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let next = action_rows(&self.get_task_history(task_id)?, &grant.cycle).len() + 1;
                row.key = format!("resident:{task_id}:{}:{next}", grant.cycle);
                self.append_resident_action_row(task_id, &row)
            })?;
        Ok(ResidentCallAdmission::Ledgered(ResidentActionTicket {
            task_id: task_id.to_string(),
            row,
        }))
    }

    fn append_resident_action_row(
        &self,
        task_id: &str,
        row: &ResidentActionRow,
    ) -> Result<(), OrbitError> {
        let note = serde_json::to_string(row)
            .map_err(|error| OrbitError::Execution(format!("serialize action row: {error}")))?;
        self.append_resident_history(task_id, RESIDENT_ACTION_EVENT, note)
    }

    /// Record how a ledgered write ended. The key travels back in the
    /// response so the cycle can cite it in its checkpoint.
    pub(crate) fn close_resident_action(
        &self,
        ticket: ResidentActionTicket,
        result: &mut Result<Value, OrbitError>,
    ) -> Result<(), OrbitError> {
        let ResidentActionTicket { task_id, mut row } = ticket;
        row.phase = match result {
            Ok(_) => ResidentActionPhase::Ok,
            Err(OrbitError::PolicyDenied(_)) => ResidentActionPhase::Refused,
            Err(_) => ResidentActionPhase::Failed,
        };
        if let Ok(value) = result {
            row.reference = ["run_id", "id", "task_id"]
                .into_iter()
                .find_map(|key| {
                    value
                        .get(key)
                        .or_else(|| value.get("task").and_then(|task| task.get(key)))
                        .and_then(Value::as_str)
                })
                .map(str::to_string);
            if let Value::Object(map) = value {
                map.insert("resident_action_key".into(), json!(row.key));
            }
        }
        self.append_resident_action_row(&task_id, &row)
    }
}
