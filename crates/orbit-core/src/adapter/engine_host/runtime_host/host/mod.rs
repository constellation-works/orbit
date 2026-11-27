//! The `RuntimeHost` capability traits implemented for [`EngineHost`], one
//! file per trait.

mod dispatch;

mod environment;

mod lifecycle;

mod private_vcs;

mod recovery;

mod review;

use std::sync::Arc;

use orbit_engine::{RunSettings, RuntimeHostProcessEnvironment};

use crate::OrbitRuntime;
use crate::application::contracts::EngineHostPort;

/// Adapter that implements `orbit-engine`'s host traits over a borrowed
/// [`OrbitRuntime`], holding the run settings resolved when it was built.
#[derive(Clone)]
pub struct EngineHost<'a> {
    runtime: &'a OrbitRuntime,
    run_settings: Arc<RunSettings>,
}

impl<'a> EngineHost<'a> {
    /// Wrap `runtime` in the engine-host adapter, resolving its run settings
    /// once.
    ///
    /// Every field is fixed for the adapter's life: each is either a constant
    /// of this build or a value `runtime` already holds (configuration,
    /// paths, the workspace binding and the actor), and `runtime` is borrowed
    /// for as long as the adapter lives.
    pub fn new(runtime: &'a OrbitRuntime) -> Self {
        let settings = runtime.settings();
        let run_settings = RunSettings {
            completion_hold_rules: crate::runtime::RuntimeKernel::completion_hold_rules(runtime)
                .to_vec(),
            workspace_base_branch: crate::runtime::RuntimeKernel::workspace_base_branch(runtime)
                .to_string(),
            unlocked_path_validation: orbit_engine::UnlockedPathValidation {
                paths: settings.unlocked_context_paths().patterns().to_vec(),
                commands: settings.unlocked_path_validation_commands().to_vec(),
            },
            generated_paths: settings
                .generated_paths()
                .iter()
                .map(|(path, command)| orbit_engine::GeneratedPath {
                    path: path.clone(),
                    command: command.clone(),
                })
                .collect(),
            host_os: orbit_types::workflow::EvidenceHostOs::current(),
            agent_stall_window: Some(std::time::Duration::from_secs(
                orbit_engine::AGENT_STALL_MINUTES * 60,
            )),
            activity_wall_clock_timeouts: settings.activity_wall_clock_timeouts().clone(),
            requires_claude_worker_credential: cfg!(target_os = "macos"),
            validation_build_output_env: runtime.validation_env_policy().build_output_env.clone(),
            orbit_registry_root: Some(runtime.paths().global_dir.to_string_lossy().into_owned()),
            orbit_workspace_selector: runtime
                .workspace_runtime_binding()
                .map(|binding| binding.logical_workspace_id.trim())
                .filter(|selector| !selector.is_empty())
                .map(ToOwned::to_owned),
            repo_root: Some(runtime.paths().repo_root.to_string_lossy().into_owned()),
            worktree_reclaim_patterns: settings.worktree_reclaim().to_vec(),
            worktree_parked_reclaim_after_hours: Some(
                settings.worktree_parked_reclaim_after_hours(),
            ),
            data_root: runtime.data_root_path().to_path_buf(),
            actor_model_identity: matches!(
                runtime.actor().kind,
                crate::runtime::context::ActorKind::Agent
            )
            .then(|| runtime.actor_label().trim())
            .filter(|label| !label.is_empty())
            .map(ToOwned::to_owned),
            pr_config: crate::runtime::RuntimeKernel::pr_config(runtime).clone(),
        };
        Self {
            runtime,
            run_settings: Arc::new(run_settings),
        }
    }
}

impl RuntimeHostProcessEnvironment for EngineHost<'_> {
    fn process_env(&self) -> orbit_common::Env {
        self.runtime.env().clone()
    }
}

impl EngineHostPort for OrbitRuntime {
    type Host<'a> = EngineHost<'a>;

    fn engine_host(&self) -> EngineHost<'_> {
        EngineHost::new(self)
    }
}
