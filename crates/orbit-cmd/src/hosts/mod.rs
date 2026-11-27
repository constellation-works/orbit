//! The host app's reads that need `orbit-mcp` or a workspace runtime, and
//! host routing [ORB-14448].
//!
//! `orbit_core::app::host` owns the host commands and their two ports. This
//! module implements them: [`Probe`] over the federated MCP probe session,
//! and [`PullDrains`] over each replica checkout's runtime, opened read-only.
//! Routing a task id or a `--host` selector to a registered host stays here
//! too, because it opens the federated client.

mod probe;
mod remote;
mod route;

use std::path::Path;

use orbit_common::OrbitError;
use orbit_core::app::host::{HostRow, Ports, Probe, ProbedHost, PullDrainDependent, PullDrains};
use orbit_registry::hosts::HostEntry;
use orbit_types::workflow::JobRunState;
use orbit_types::workspace::{Workspace, WorkspaceCheckout};

use crate::registry_runtime::RegisteredRuntimeFactory;

pub use probe::local_host_facts;
pub use route::{
    HostWorkspaceRoute, TaskIdRoute, host_ssh_target, local_mirror_workspace, pull_selector,
    remote_task_holder, resolve_host_workspace, route_task_id, routed_client, selector_remote_host,
    task_prefix_remote,
};

static PROBE: FederatedProbe = FederatedProbe;

static PULL_DRAINS: RegisteredPullDrains = RegisteredPullDrains;

/// The live probe, over the federated MCP probe session.
struct FederatedProbe;

impl Probe for FederatedProbe {
    fn remote_row(&self, target: &HostEntry, caller_machine_id: &str, probe: bool) -> HostRow {
        remote::remote_row(target, caller_machine_id, probe)
    }

    fn probe_ssh(&self, ssh: &str, caller_machine_id: &str) -> Result<ProbedHost, OrbitError> {
        let live = probe::probe_ssh(ssh, caller_machine_id)?;
        let facts = live.facts;
        Ok(ProbedHost {
            skew_fields: remote::skew_fields(&facts),
            workspaces: live
                .workspaces
                .iter()
                .map(|workspace| remote::remote_workspace(workspace, &facts.machine_id))
                .collect(),
            machine_id: facts.machine_id,
            machine_name: facts.machine_name,
            task_prefix: facts.task_prefix,
            binary_version: facts.binary_version,
            protocol_fingerprint: facts.protocol_fingerprint,
            host_os: facts.host_os,
        })
    }
}

/// The pull-drain read, over each replica checkout's runtime.
struct RegisteredPullDrains;

impl PullDrains for RegisteredPullDrains {
    fn pull_drains(
        &self,
        global_root: &Path,
        workspace: &Workspace,
        checkout: &WorkspaceCheckout,
    ) -> Vec<PullDrainDependent> {
        let runtime = match RegisteredRuntimeFactory::open_registered_checkout_read_only(
            global_root,
            workspace,
            checkout,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, workspace = %workspace.id, "cannot inspect pull drains");
                return Vec::new();
            }
        };
        let mut drains = Vec::new();
        for state in [JobRunState::Running, JobRunState::Pending] {
            let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
                job_id: Some(orbit_core::application::contracts::PULL_DRAIN_JOB.to_string()),
                state: Some(state),
                ..Default::default()
            });
            match runs {
                Ok(runs) => drains.extend(runs.into_iter().map(|run| PullDrainDependent {
                    workspace_id: workspace.id.clone(),
                    run_id: run.run_id,
                    state: run.state.to_string(),
                })),
                Err(error) => {
                    tracing::warn!(%error, workspace = %workspace.id, "cannot list pull drains");
                }
            }
        }
        drains
    }
}

/// The host app's ports, as a surface passes them to `orbit_core::app::host`.
pub fn ports() -> Ports<'static> {
    Ports {
        probe: &PROBE,
        pull_drains: &PULL_DRAINS,
    }
}
