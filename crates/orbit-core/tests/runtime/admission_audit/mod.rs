//! Existing audit cases at exported helper and composed-runtime boundaries.
use orbit_core::application::auto_tasks::AutoTaskAddParams;
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};

pub(super) use task_support::enter_isolated_child as isolate;

mod authorization;
mod auto_task_crud;
mod auto_task_scheduler;
mod command_tools;
mod config_path;
mod definition_fixture;
mod desktop_authorization;
mod desktop_support;
mod desktop_write;
mod final_recovery;
mod handoff;
mod init;
mod isolation;
mod lock_commands;
mod plugin_build;
mod plugin_conformance;
mod plugin_fixture;
mod plugin_host;
mod plugin_install;
mod plugin_secrets;
mod plugin_workspace;
mod resource_fixture;
mod resource_pressure;
mod resource_probe;
mod secret_store;
mod task_add;
mod task_support;
mod task_tools;
mod tool_support;
mod workflow_tools;
mod workspace_claim;

/// Add-params for a definition on an N-minute interval schedule.
pub(crate) fn interval_params(name: &str, every_minutes: u64) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Auto-task {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes },
        template: template(&format!("Chore for {name}")),
        dedupe: DedupePolicy::SkipIfOpen,
    }
}

use orbit_types::task::{TaskPriority, TaskType};
/// A minimal, valid template for tests.
pub(super) fn template(title: &str) -> AutoTaskTemplate {
    AutoTaskTemplate {
        title: title.to_string(),
        description: "Recurring chore body.".to_string(),
        acceptance_criteria: vec!["Chore is observable.".to_string()],
        task_type: TaskType::Chore,
        tags: vec![],
        required_tools: Vec::new(),
        context_files: Vec::new(),
        priority: TaskPriority::Medium,
        complexity: None,
        crew: None,
        status: orbit_types::task::TaskStatus::Backlog,
    }
}

mod dependabot;

mod pilot_persist;

mod clock_settings;

mod run_failure;

mod engine_support;

mod store_support;

mod artifact_health;

mod plugin_remove;

#[cfg(unix)]
mod cancel_identity;

mod run_conflict;

mod agent_invoke;

mod agent_explore;

mod disk_probe;

mod run_support;

mod review_admit;

mod review_correction;

mod review_judgement;

mod review_settle;

mod review_support;

mod sandbox_resolve;

#[cfg(target_os = "macos")]
mod sandbox_git_metadata;

mod contention;

mod pilot_approval;

mod callback;

mod managed_assets;

mod lock_overlap;

mod recovery_git;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod codex_side_roots;
