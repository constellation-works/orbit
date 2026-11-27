//! Resolved run-level configuration supplied to the engine as one value.
//!
//! Each field here replaces a resolved-configuration getter that previously
//! lived on [`crate::HostEnvironment`]. Methods that perform I/O, mutate state,
//! query durable stores, or whose answer can change during a run stay on
//! [`crate::HostEnvironment`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::workflow::{
    CompletionHoldRule, EvidenceHostOs, HostValidationGates, ValidationGates,
};

use crate::context::{AGENT_STALL_MINUTES, GeneratedPath, PrConfig, UnlockedPathValidation};

/// Default reclaim patterns for a host without explicit `[worktree] reclaim`
/// configuration.
const DEFAULT_WORKTREE_RECLAIM_PATTERNS: [&str; 3] =
    ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"];

/// Resolved host and workspace configuration for an engine run.
///
/// Composition resolves these values once from the captured environment,
/// workspace binding, and configuration; the engine reads fields directly
/// rather than invoking individual configuration getters on the host trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSettings {
    /// Commands and review baselines this host holds a candidate to, with
    /// provenance (`workflow.required_validation_commands`,
    /// `review.baseline_commands`, `workflow.light_delivery_paths`,
    /// `workflow.review_skip_tags`; or the owner's captured gates on a
    /// claimed leaf).
    pub validation_gates: HostValidationGates,
    /// `completion.hold`: admitted rules, in configured order, that keep a
    /// completion-authorized run's task in `review`. Empty holds nothing.
    pub completion_hold_rules: Vec<CompletionHoldRule>,
    /// The branch this workspace integrates into: the registered workspace
    /// base branch, else `workflow.base_branch` (default `"main"`).
    pub workspace_base_branch: String,
    /// `workflow.unlocked_context_paths` and
    /// `workflow.unlocked_path_validation_commands`: narrowed validation for
    /// repairs confined to unlocked paths.
    pub unlocked_path_validation: UnlockedPathValidation,
    /// `workflow.generated_paths`: conflicting paths regenerated with a
    /// command instead of merged during conflict recovery.
    pub generated_paths: Vec<GeneratedPath>,
    /// The OS this host runs implementers on, which decides the deferral kinds
    /// it admits.
    pub host_os: Option<EvidenceHostOs>,
    /// How long a provider process may produce no output and use no CPU before
    /// its step fails as stalled. `None` disables the check.
    pub agent_stall_window: Option<Duration>,
    /// Workspace overrides for per-activity CLI wall-clock budgets, in seconds.
    pub activity_wall_clock_timeouts: BTreeMap<String, u64>,
    /// Whether a Claude activity must be launched with its own worker
    /// credential rather than the shared Claude Desktop login.
    pub requires_claude_worker_credential: bool,
    /// Workspace output bindings (`workflow.validation_env.build_output_env`)
    /// for cache identity, without probing a login shell.
    pub validation_build_output_env: Vec<String>,
    /// Authoritative shared Orbit registry root (`ORBIT_REGISTRY_ROOT`) handed
    /// to spawned CLI agents.
    pub orbit_registry_root: Option<String>,
    /// Trusted logical `ws_*` selector (`ORBIT_WORKSPACE`) handed to spawned
    /// CLI agents.
    pub orbit_workspace_selector: Option<String>,
    /// Resolved repository root path for the workspace, if the host has one.
    pub repo_root: Option<String>,
    /// Admitted rebuildable paths for worktree reclamation.
    pub worktree_reclaim_patterns: Vec<String>,
    /// How long a task must have been parked in `someday` before scheduled
    /// worktree GC may reclaim its run's worktree. `None` keeps parked trees.
    pub worktree_parked_reclaim_after_hours: Option<u64>,
    /// Resolved Orbit data root directory for host caches and state files.
    pub data_root: PathBuf,
    /// Current agent model identity when operating as an agent, or `None` when
    /// there is no model-bearing actor.
    pub actor_model_identity: Option<String>,
    /// Pull-request, commit, branch, and CI infrastructure retry conventions.
    pub pr_config: PrConfig,
}

impl RunSettings {
    /// Workspace override for `activity_name`'s CLI wall-clock budget, in
    /// seconds.
    pub fn activity_wall_clock_timeout_seconds(&self, activity_name: &str) -> Option<u64> {
        self.activity_wall_clock_timeouts
            .get(activity_name)
            .copied()
    }

    /// The resolved repository root path, or the standard unavailable error
    /// when this host supplies none.
    pub fn repo_root(&self) -> Result<&str, OrbitError> {
        self.repo_root.as_deref().ok_or_else(|| {
            OrbitError::Execution("runtime host capability 'repo_root' is unavailable".into())
        })
    }

    /// Commands every delivered candidate must pass
    /// (`validation_gates.gates.required_validation_commands`).
    pub fn required_validation_commands(&self) -> &[String] {
        &self.validation_gates.gates.required_validation_commands
    }

    /// The owner's review baseline commands
    /// (`validation_gates.gates.baseline_commands`).
    pub fn review_baseline_commands(&self) -> &[String] {
        &self.validation_gates.gates.baseline_commands
    }
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            validation_gates: HostValidationGates::local(ValidationGates {
                required_validation_commands: Vec::new(),
                baseline_commands: Vec::new(),
                light_delivery_paths: None,
                review_skip_tags: None,
            }),
            completion_hold_rules: Vec::new(),
            workspace_base_branch: "main".to_string(),
            unlocked_path_validation: UnlockedPathValidation::default(),
            generated_paths: Vec::new(),
            host_os: EvidenceHostOs::current(),
            agent_stall_window: Some(Duration::from_secs(AGENT_STALL_MINUTES * 60)),
            activity_wall_clock_timeouts: BTreeMap::new(),
            requires_claude_worker_credential: false,
            validation_build_output_env: Vec::new(),
            orbit_registry_root: None,
            orbit_workspace_selector: None,
            repo_root: None,
            worktree_reclaim_patterns: DEFAULT_WORKTREE_RECLAIM_PATTERNS.map(String::from).to_vec(),
            worktree_parked_reclaim_after_hours: None,
            data_root: PathBuf::new(),
            actor_model_identity: None,
            pr_config: PrConfig::default(),
        }
    }
}
