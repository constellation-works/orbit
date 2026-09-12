//! The host port this crate's scheduling domain evaluates against.
//!
//! Automation owns the rules; the host owns identity, paths, stores, task and
//! run lifecycle, and every authorization decision. One trait states exactly
//! which host capabilities the domain consumes, so the crate stays free of a
//! runtime (and of a dependency edge back onto Core). Core implements it once
//! for `OrbitRuntime`; tests may implement it for a double.
//!
//! Signatures are deliberately data-shaped: a module that needs one path or
//! one query gets that path or that query, not a whole paths bundle or store
//! registry. The single exception — the automation store, which the shared
//! evaluators drive transactions on — says so on its method.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_store::contracts::{AutomationStoreBackend, TaskCandidates, TaskListFilter};
use orbit_types::identity::Crew;
use orbit_types::task::{
    ArtifactManifestFileV2, Task, TaskAddParams, TaskArtifact, TaskHistoryEntry,
};
use orbit_types::workflow::automation::members::StateTrigger;
use orbit_types::workflow::automation::{AutomationState, SourcePage};
use orbit_types::workflow::{JobRun, PipelineState};
use serde_json::Value;

use crate::error::AutomationError;
use crate::members::MemberConstraints;
use crate::source::Source;

/// Host-supplied owner facts about a dispatched run, asked independently of
/// the run's persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOwnerLiveness {
    /// The recorded owner process is still executing.
    Alive,
    /// The recorded owner process has stopped.
    Stopped,
    /// Liveness could not be established; not evidence either way.
    Unknown,
}

/// One workspace's host capabilities, as the scheduling domain consumes them.
pub trait AutomationHost {
    // ---- identity and paths ----

    /// Checkout root every source observation and Git command runs against.
    fn repo_root(&self) -> &Path;

    /// This checkout's own `.orbit` directory, which owns tracked definition
    /// files (`auto_tasks/`, `routines/`). In a linked worktree this is that
    /// worktree's directory, not the registered primary checkout's.
    fn local_orbit_dir(&self) -> PathBuf;

    /// The `.orbit` directory shared by every linked worktree of this
    /// checkout — the dispatch root a routine source is identified by.
    fn shared_orbit_dir(&self) -> PathBuf;

    /// Host-local coordination state directory holding scheduler cursors.
    fn state_dir(&self) -> PathBuf;

    /// Stable logical id of the workspace this host is bound to.
    fn workspace_id(&self) -> Result<String, OrbitError>;

    /// This machine's registered identity, or `None` on an unregistered host
    /// (where no delivery consumer can be owned).
    fn machine_identity(&self) -> Option<&str>;

    /// Registered owner machine of this workspace, from the registry record.
    fn workspace_owner_machine_id(&self) -> Option<&str>;

    /// Declared remote owner when this checkout is a replica, else `None`.
    fn coordination_write_owner(&self) -> Option<&str>;

    /// The audit label this host attributes its own writes to.
    fn write_label(&self) -> Result<String, OrbitError>;

    // ---- stores ----

    /// Consumer state, claims, receipts, waivers and delivery intents.
    ///
    /// Exposed whole: the evaluators are handed this backend and drive their
    /// own transactions on it, so narrowing it per query would just re-declare
    /// the Store contract.
    fn automation_store(&self) -> Result<Arc<dyn AutomationStoreBackend>, OrbitError>;

    // ---- tasks ----

    /// One task document by id.
    fn get_task(&self, id: &str) -> Result<Task, OrbitError>;

    /// Status-transition history for one task, oldest first.
    fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError>;

    /// One task artifact by path, or `None` when it was never submitted.
    fn get_task_artifact(&self, id: &str, path: &str) -> Result<Option<TaskArtifact>, OrbitError>;

    /// Artifact provenance (author, digest) for one task.
    fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactManifestFileV2>, OrbitError>;

    /// Bounded candidate page for a filter, with the matching total.
    fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError>;

    /// Every task carrying all of `tags`.
    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError>;

    /// Create a task through the host's ordinary creation path.
    fn add_task(&self, params: TaskAddParams) -> Result<Task, OrbitError>;

    /// Create a task admitted under `action_key`, so a retried admission
    /// cannot mint a second task for the same claim.
    fn add_task_admitted(
        &self,
        params: TaskAddParams,
        action_key: &str,
    ) -> Result<Task, OrbitError>;

    // ---- runs ----

    /// One run as the host reports it, including the host's own stale-run
    /// reconciliation side effects. Automation must observe the reconciled
    /// view rather than raw persisted state.
    fn show_job_run(&self, run_id: &str) -> Result<JobRun, OrbitError>;

    /// One run read straight from the store, without reconciliation, or
    /// `None` when the id is unknown.
    fn job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;

    /// Runs retrying `run_id`, up to `limit`.
    fn job_run_retries(&self, run_id: &str, limit: usize) -> Result<Vec<JobRun>, OrbitError>;

    /// Persisted pipeline state for a run, when it has one.
    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;

    /// Whether a run's recorded owner process is still executing.
    fn run_owner_liveness(&self, run: &JobRun) -> RunOwnerLiveness;

    /// The run already admitted under `action_key`, if any.
    fn automation_job_for_key(&self, action_key: &str) -> Result<Option<String>, OrbitError>;

    /// Submit `job_name` for an automation claim under `action_key`,
    /// returning the run id.
    fn submit_automation_run(
        &self,
        job_name: &str,
        input: Value,
        action_key: &str,
    ) -> Result<String, OrbitError>;

    /// Submit `job_name` as a routine fire for the source workspace rooted at
    /// `source_orbit_dir`, returning the run id. The host owns the dispatch
    /// input contract and the routine trigger record. `slot` is the RFC 3339
    /// scheduled slot this fire consumes.
    fn submit_routine_run(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError>;

    /// Whether `job_name` resolves in this workspace's job catalog, so an
    /// unresolvable routine target is a load error and not a fire-time
    /// surprise.
    fn job_target_resolves(&self, job_name: &str) -> bool;

    // ---- authorization and composition ----

    /// Refuse coordination writes this checkout does not own.
    fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError>;

    /// Operation-mode constraints for a state trigger. Empty constraints are
    /// the pre-operation-mode behavior; Automation never reads grants itself.
    fn member_constraints(&self, trigger: &StateTrigger) -> Result<MemberConstraints, OrbitError>;

    /// Apply proven before-PR review exclusions to an observed source page.
    /// The shared rules live in [`crate::review`]; the host owns the persisted
    /// certificates they are proven against.
    fn review_exclusions(
        &self,
        source: &Source<'_>,
        state: &AutomationState,
        page: &mut SourcePage,
    ) -> Result<(), AutomationError>;

    /// The crew a task would actually run under — part of its material input.
    fn effective_crew(&self, task_crew: Option<&str>) -> Result<Crew, OrbitError>;

    /// Reject required tools this host cannot grant.
    fn validate_required_tools(&self, required_tools: &[String]) -> Result<(), OrbitError>;

    /// Reject a crew name this host cannot resolve.
    fn validate_crew_name(&self, crew: Option<&str>) -> Result<(), OrbitError>;

    /// Provider CLI arguments that list the pull requests containing `commit`.
    /// Provider-tool knowledge stays with the host; Automation only runs the
    /// command under its own source budget.
    fn provider_pull_requests_argv(
        &self,
        repository: &str,
        commit: &str,
    ) -> Result<Vec<String>, OrbitError>;

    /// Refresh this workspace's read-side token projection. A stale
    /// projection must not stop schedule evaluation.
    fn refresh_token_scoreboard(&self) -> Result<(), OrbitError>;
}
