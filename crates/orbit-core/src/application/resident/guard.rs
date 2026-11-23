//! The resident cycle's tool boundary: what a cycle's agent may call, and
//! the action ledger that makes a recovery cycle skip what its dead
//! predecessor already did.
//!
//! A run scope naming a `resident_cycle_pipeline` run subjects its calls to
//! the cycle guard. Exercising a resident right on an operator-only operation
//! also requires the broker to authenticate that run. Reads always pass. Every
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
use orbit_common::governance::{operation::OperationVerb, task::TaskVerb};
use orbit_common::protocol::tool_input::optional_csv_or_string_list_alias;
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
use crate::runtime::workspace::catalog::{FederatedWorkspaceTarget, WorkspaceScope};

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
pub(crate) struct ResidentActionTicket {
    ledger_runtime: Option<Box<OrbitRuntime>>,
    task_id: String,
    row: ResidentActionRow,
}

impl std::fmt::Debug for ResidentActionTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentActionTicket")
            .field("cross_workspace", &self.ledger_runtime.is_some())
            .field("task_id", &self.task_id)
            .field("row", &self.row)
            .finish()
    }
}

impl ResidentActionTicket {
    pub(crate) fn resident_action_key(&self) -> &str {
        &self.row.key
    }
}

std::thread_local! {
    /// The key of the most recent ledgered write on this thread that
    /// failed or was refused. The broker dispatch on the same thread
    /// takes it to fill the envelope `action_key` (ORB-25284). The CLI
    /// also takes it for JSON errors; each dispatch clears stale keys first.
    static FAILED_RESIDENT_ACTION_KEY: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Drop any stale ledger key left on this thread.
pub(crate) fn clear_failed_resident_action_key() {
    FAILED_RESIDENT_ACTION_KEY.with(|slot| *slot.borrow_mut() = None);
}

/// Take the ledger key a ledgered write on this thread failed with.
pub fn take_failed_resident_action_key() -> Option<String> {
    FAILED_RESIDENT_ACTION_KEY.with(|slot| slot.borrow_mut().take())
}

fn record_failed_resident_action_key(key: &str) {
    FAILED_RESIDENT_ACTION_KEY.with(|slot| *slot.borrow_mut() = Some(key.to_string()));
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

fn digest_of(bytes: &[u8]) -> String {
    orbit_types::identity::lower_hex(Sha256::digest(bytes))
}

/// `sha256:v2:` over the call's canonical input (object keys sorted
/// recursively, array order preserved).
fn input_digest(tool: &str, input: &Value) -> String {
    let canonical = serde_json::to_vec(&json!([
        tool,
        orbit_common::protocol::sorted_keys_json_value(input)
    ]))
    .unwrap_or_default();
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

/// A `field` projection of one field answers that field's bare value, which
/// has no object to carry the `resident_action_key` a cycle cites. Refuse it
/// before the ledger opens, so the write never runs unrecorded. A `fields`
/// list, or a `field` naming several fields, answers an object and keeps it.
fn refuse_scalar_field_projection(tool: &str, input: &Value) -> Result<(), OrbitError> {
    if input.get("fields").is_some() {
        return Ok(());
    }
    let projection = optional_csv_or_string_list_alias(input, &["fields", "field"])?;
    if projection.is_some_and(|fields| fields.len() == 1) {
        return Err(refused(
            "resident_projection_refused",
            format!(
                "`{tool}` with a `field` projection answers a bare value, which carries \
                 no `resident_action_key`; request the field in a `fields` list instead"
            ),
        ));
    }
    Ok(())
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

/// Friction update is the one resident write a cycle may also call through
/// another covered workspace's runtime on the same host.
const RESIDENT_FRICTION_UPDATE_TOOL: &str = "orbit.friction.update";

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
    /// Route a brokered recovery to the named, covered owner on this host.
    /// Check the home grant before opening any other workspace.
    pub(crate) fn resident_recovery_workspace(
        &self,
        run_id: &str,
        input: &Value,
    ) -> Result<Self, OrbitError> {
        let Some(selector) = input.get("workspace").and_then(Value::as_str) else {
            return Ok(self.clone());
        };
        let (home_ids, grant) = self
            .live_cycle_home_grant_local(run_id)
            .ok_or_else(|| refused("resident_cycle_stale", "no live resident cycle"))?;
        let in_home = !self.resident_workspace_ids(selector).is_disjoint(&home_ids);
        let allowed = self.covered_workspaces_from_grant(home_ids, &grant);
        self.ensure_named_workspaces_in_scope(&allowed, input)?;
        require_right(
            &grant,
            TaskVerb::ResumeDelivery.tool_name(),
            ResidentRight::Recover,
        )?;
        if in_home {
            return Ok(self.clone());
        }
        let catalog = self
            .workspace_catalog()
            .ok_or_else(|| refused("resident_scope_refused", "no registered workspace catalog"))?;
        let targets =
            catalog.resolve_scope(&WorkspaceScope::Selectors(vec![selector.to_string()]))?;
        let [target] = targets.as_slice() else {
            return Err(refused(
                "resident_scope_refused",
                "recovery requires one local owner workspace",
            ));
        };
        if !allowed.contains(&target.workspace_id) {
            return Err(refused(
                "resident_scope_refused",
                format!("workspace `{selector}` is outside the resident mandate's authority"),
            ));
        }
        let runtime = Self::from_kernel(catalog.open(target)?);
        Ok(runtime.with_workspace_catalog(catalog.clone()))
    }

    /// Locate the registered workspace holding `run_id` as a
    /// `resident_cycle_pipeline` run when `self` does not hold it.
    fn resident_cycle_home_target(&self, run_id: &str) -> Option<FederatedWorkspaceTarget> {
        let catalog = self.workspace_catalog()?;
        let store = self.sqlite_store().ok()?;
        let current_ids = self.canonical_workspace_ids().ok()?;
        catalog
            .resolve_scope(&WorkspaceScope::AllRegistered)
            .ok()?
            .into_iter()
            .filter(|target| !current_ids.contains(&target.workspace_id))
            .find(|target| {
                orbit_store::compose::workspace_job_run_store(
                    store.clone(),
                    target.workspace_id.clone(),
                )
                .get_job_run(run_id)
                .ok()
                .flatten()
                .is_some_and(|run| run.job_id == RESIDENT_CYCLE_JOB)
            })
    }

    /// Canonical workspace IDs for this runtime (its partition ID plus, when
    /// bound to a registered checkout, its logical catalog ID and partition
    /// ID). Never includes checkout basenames or registered names.
    fn canonical_workspace_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let mut ids = BTreeSet::from([self.workspace_id()?]);
        if let Some(binding) = self.workspace_runtime_binding() {
            ids.insert(binding.logical_workspace_id.clone());
            ids.insert(binding.task_partition_id.clone());
        }
        Ok(ids)
    }

    /// Canonical workspace IDs that `selector` resolves to: this runtime's
    /// canonical IDs when `selector` names one of them, otherwise the
    /// canonical workspace ID resolved by the catalog. Checkout directory
    /// labels and registered names are never included as IDs, and unknown or
    /// ambiguous selectors return an empty set.
    fn resident_workspace_ids(&self, selector: &str) -> BTreeSet<String> {
        let selector = selector.trim();
        if selector.is_empty() {
            return BTreeSet::new();
        }
        let own_ids = self.canonical_workspace_ids().unwrap_or_default();
        if own_ids.contains(selector) {
            return own_ids;
        }
        let mut ids = BTreeSet::new();
        if let Some(catalog) = self.workspace_catalog()
            && let Ok(targets) =
                catalog.resolve_scope(&WorkspaceScope::Selectors(vec![selector.to_string()]))
            && targets.len() == 1
        {
            let target = &targets[0];
            if own_ids.contains(&target.workspace_id) {
                ids.extend(own_ids.iter().cloned());
            } else {
                ids.insert(target.workspace_id.clone());
            }
        }
        ids
    }

    fn live_cycle_home_grant_local(
        &self,
        run_id: &str,
    ) -> Option<(BTreeSet<String>, ResidentCycleGrant)> {
        let run = self.show_job_run_observed(run_id).ok()?;
        if run.job_id != RESIDENT_CYCLE_JOB {
            return None;
        }
        // History reads recover interrupted task commits. Observational reads
        // fail closed when non-repairing inspection cannot verify that the home
        // partition is settled before reading the grant.
        self.inspect_live_execution_claims().ok()?;
        let task_id = self.resident_cycle_mandate_of_run(run_id).ok()?;
        let history = self.get_task_history(&task_id).ok()?;
        let grant = live_cycle_from_history(&history)
            .ok()?
            .filter(|live| live.run_id == run_id && live.expires_at > Utc::now())
            .and_then(|live| cycle_grant(&history, &live.cycle))?;
        Some((self.canonical_workspace_ids().ok()?, grant))
    }

    fn live_cycle_home_grant(
        &self,
        run_id: &str,
    ) -> Option<(BTreeSet<String>, ResidentCycleGrant)> {
        let run_id = run_id.trim();
        if run_id.is_empty() {
            return None;
        }
        match self.show_job_run_observed(run_id) {
            Ok(_) => self.live_cycle_home_grant_local(run_id),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::JobRun,
                ..
            }) => {
                let catalog = self.workspace_catalog()?;
                let target = self.resident_cycle_home_target(run_id)?;
                let foreign = Self::from_kernel(catalog.open_observational(&target).ok()?);
                let (mut home_ids, grant) = foreign.live_cycle_home_grant_local(run_id)?;
                home_ids.insert(target.workspace_id);
                Some((home_ids, grant))
            }
            Err(_) => None,
        }
    }

    fn covered_workspaces_from_grant(
        &self,
        home_ids: BTreeSet<String>,
        grant: &ResidentCycleGrant,
    ) -> BTreeSet<String> {
        let mut covered = home_ids;
        for selector in &grant.workspaces {
            covered.extend(self.resident_workspace_ids(selector));
        }
        covered
    }

    /// Whether `run_id` holds a mandate's live cycle in this workspace or
    /// another workspace in the registered catalog.
    pub(crate) fn is_live_resident_cycle_run(&self, run_id: &str) -> bool {
        self.live_cycle_home_grant(run_id).is_some()
    }

    /// Whether `run_id` is a live resident cycle allowed to read drain status
    /// (`orbit.workflow.auto` action `status`) for the workspace `self` and
    /// `input` address. Returns `Ok(false)` when `run_id` is not a live cycle
    /// so non-resident and stale callers fall through to operator capability.
    pub(crate) fn resident_cycle_drain_status_allowed(
        &self,
        run_id: &str,
        input: &Value,
    ) -> Result<bool, OrbitError> {
        let Some((home_ids, grant)) = self.live_cycle_home_grant(run_id) else {
            return Ok(false);
        };
        let covered = self.covered_workspaces_from_grant(home_ids, &grant);
        let current_ids = self.canonical_workspace_ids()?;
        if current_ids.is_disjoint(&covered) {
            return Err(refused(
                "resident_scope_refused",
                format!(
                    "workspace `{}` is outside the resident mandate's authority",
                    self.workspace_label()
                ),
            ));
        }
        self.ensure_named_workspaces_in_scope(&covered, input)?;
        Ok(true)
    }

    /// The canonical workspace IDs a live resident cycle may read runtime
    /// attention for: its mandate's own workspace and the ones its frozen
    /// grant covers. `None` for any session that is not the live cycle of a
    /// mandate: no run binding, another job's run, a stale or expired cycle,
    /// a pending task commit, or a lookup that fails. The answer widens no
    /// write: it only lets `orbit.attention` read the runtime kinds for these
    /// workspaces.
    pub(crate) fn resident_cycle_attention_scope(
        &self,
        run_id: Option<&str>,
    ) -> Option<BTreeSet<String>> {
        let run_id = run_id.map(str::trim).filter(|id| !id.is_empty())?;
        let (home_ids, grant) = self.live_cycle_home_grant_local(run_id)?;
        Some(self.covered_workspaces_from_grant(home_ids, &grant))
    }

    /// Admit the call before dispatch. Only the authenticated resident broker may
    /// substitute a cycle's grant for operator authority.
    pub(crate) fn admit_resident_cycle_call(
        &self,
        run_id: Option<&str>,
        brokered_resident: bool,
        tool: &str,
        class: ResidentCallClass,
        input: &Value,
    ) -> Result<ResidentCallAdmission, OrbitError> {
        let right_gated =
            crate::runtime::authorization::is_resident_cycle_right_gated_operation(tool);
        if right_gated && run_id.is_some() && !brokered_resident {
            return Err(OrbitError::CapabilityDenied(format!(
                "a managed run may call '{tool}' only through its resident cycle's broker"
            )));
        }
        let admission = self.check_resident_cycle_grant(run_id, tool, class, input)?;
        if right_gated && brokered_resident && matches!(admission, ResidentCallAdmission::Proceed) {
            return Err(OrbitError::CapabilityDenied(format!(
                "the resident cycle did not admit '{tool}'"
            )));
        }
        Ok(admission)
    }

    fn check_resident_cycle_grant(
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
                    if class == ResidentCallClass::Read {
                        return Ok(ResidentCallAdmission::Proceed);
                    }
                    let Some(target) = self.resident_cycle_home_target(run_id) else {
                        return Ok(ResidentCallAdmission::Proceed);
                    };
                    if tool != RESIDENT_FRICTION_UPDATE_TOOL
                        && tool != TaskVerb::ResumeDelivery.tool_name()
                    {
                        return Err(refused(
                            "resident_scope_refused",
                            "a resident cycle writes only through its mandate's own workspace",
                        ));
                    }
                    let mut home = self
                        .workspace_catalog()
                        .ok_or_else(|| {
                            refused(
                                "resident_lookup_refused",
                                format!(
                                    "missing workspace catalog for resident cycle run `{run_id}`"
                                ),
                            )
                        })?
                        .open(&target)
                        .map(Self::from_kernel)
                        .map_err(|error| {
                            refused(
                                "resident_lookup_refused",
                                format!(
                                    "could not verify the resident cycle run `{run_id}`: {error}"
                                ),
                            )
                        })?;
                    if home.workspace_catalog().is_none()
                        && let Some(catalog) = self.workspace_catalog()
                    {
                        home = home.with_workspace_catalog(catalog.clone());
                    }
                    return home
                        .check_resident_cycle_grant_for(self, run_id, tool, class, input, true);
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
        self.check_resident_cycle_grant_for(self, run_id, tool, class, input, false)
    }

    fn check_resident_cycle_grant_for(
        &self,
        target: &OrbitRuntime,
        run_id: &str,
        tool: &str,
        class: ResidentCallClass,
        input: &Value,
        cross_workspace: bool,
    ) -> Result<ResidentCallAdmission, OrbitError> {
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
        let allowed = self.covered_workspaces_from_grant(self.canonical_workspace_ids()?, &grant);
        if cross_workspace {
            let target_ids = target.canonical_workspace_ids()?;
            if target_ids.is_disjoint(&allowed) {
                let target_label = target.workspace_label();
                return Err(refused(
                    "resident_scope_refused",
                    format!(
                        "workspace `{target_label}` is outside the resident mandate's authority"
                    ),
                ));
            }
        }
        self.ensure_named_workspaces_in_scope(&allowed, input)?;
        // A grant is frozen at admission, but the deadline remains effective
        // during a live cycle, including when it is subsequently shortened.
        if matches!(
            class,
            ResidentCallClass::Gated(_) | ResidentCallClass::CompleteTask
        ) {
            let mandate = super::resident_mandate_from_artifact(self, &task_id)?
                .map_err(OrbitError::InvalidInput)?;
            if mandate
                .deadline
                .is_some_and(|deadline| deadline <= Utc::now())
            {
                return Err(refused(
                    "resident_right_refused",
                    format!("`{tool}` is unavailable after the mandate deadline"),
                ));
            }
        }
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
        self.open_resident_action(&task_id, &grant, tool, input, &history, cross_workspace)
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

    fn ensure_named_workspaces_in_scope(
        &self,
        allowed: &BTreeSet<String>,
        input: &Value,
    ) -> Result<(), OrbitError> {
        for selector in named_workspaces(input) {
            let resolved = self.resident_workspace_ids(&selector);
            if resolved.is_empty() || resolved.is_disjoint(allowed) {
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
        cross_workspace: bool,
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
        refuse_scalar_field_projection(tool, input)?;
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
                let rows = action_rows(&self.get_task_history(task_id)?, &grant.cycle);
                if tool == TaskVerb::ResumeDelivery.tool_name()
                    && rows.iter().any(|row| row.tool == tool)
                {
                    return Err(refused(
                        "resident_recovery_limit",
                        "this cycle already attempted delivery recovery",
                    ));
                }
                let next = rows.len() + 1;
                row.key = format!("resident:{task_id}:{}:{next}", grant.cycle);
                self.append_resident_action_row(task_id, &row)
            })?;
        Ok(ResidentCallAdmission::Ledgered(ResidentActionTicket {
            ledger_runtime: cross_workspace.then(|| Box::new(self.clone())),
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
    /// response (or RemoteTool error detail) so the cycle can cite it in its
    /// checkpoint. Other error variants have no compatible detail carrier,
    /// so their key is stashed for the broker envelope `action_key`.
    pub(crate) fn close_resident_action(
        &self,
        ticket: ResidentActionTicket,
        result: &mut Result<Value, OrbitError>,
    ) -> Result<(), OrbitError> {
        let ResidentActionTicket {
            ledger_runtime,
            task_id,
            mut row,
        } = ticket;
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
        if let Err(OrbitError::RemoteTool { payload, .. }) = result {
            // Keep remote diagnostics, including non-object detail, while
            // reserving the action key for this local ledger's admission.
            if !payload.is_object() {
                *payload = json!({"detail": payload.take()});
            }
            let detail = &mut payload["detail"];
            if !detail.is_object() {
                *detail = match detail.take() {
                    Value::Null => json!({}),
                    remote_detail => json!({"remote_detail": remote_detail}),
                };
            }
            detail["resident_action_key"] = json!(row.key);
        }
        if result.is_err() {
            record_failed_resident_action_key(&row.key);
        }
        let ledger = ledger_runtime.as_deref().unwrap_or(self);
        ledger.append_resident_action_row(&task_id, &row)
    }
}
