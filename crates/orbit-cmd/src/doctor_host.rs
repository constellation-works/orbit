//! The doctor app's [`Host`] port, over this crate's host registry, MCP
//! client files, registered workspaces' routines, browser kit, task-store
//! partitions and service environments.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::{Env, OrbitError};
use orbit_core::app::doctor::{
    BrowserKit, Host, KitProblem, OrphanTaskStoreRemoval, TaskStorePartition, TaskStorePartitions,
    UnreachableTaskStorePartition, WorkspaceDoctorResult,
};
use orbit_core::app::host::browser_kit;
use orbit_core::application::routines::RoutineStatusReport;
use orbit_core::bootstrap::browser_kit::KitLayout;
use orbit_types::identity::CrewEnvVar;

use crate::task_store::{
    UnclaimedPartition, inspect_task_store_partitions, remove_unclaimed_task_stores,
};

/// The composition tier's doctor host reads.
pub(crate) struct DoctorHost;

impl Host for DoctorHost {
    fn hosts_row(&self, global_root: &Path) -> WorkspaceDoctorResult {
        orbit_core::app::host::doctor_hosts_row(crate::hosts::ports().probe, global_root)
    }

    fn registered_mcp_clients(
        &self,
        repo_root: &Path,
        workspace_id: Option<&str>,
        home_dir: Option<&Path>,
        env: &Env,
    ) -> Vec<String> {
        crate::mcp_clients::registered_clients_for_workspace(repo_root, workspace_id, home_dir, env)
    }

    fn routine_statuses(&self, global_root: &Path) -> Result<RoutineStatusReport, OrbitError> {
        crate::registry_routines::routine_statuses(global_root)
    }

    fn crew_source_environment_warnings(
        &self,
        global_root: &Path,
        crews: &[(String, Vec<CrewEnvVar>)],
        env: &Env,
    ) -> Vec<String> {
        crate::service_environment::crew_source_environment_warnings(global_root, crews, env)
    }

    fn browser_kit(&self, layout: &KitLayout, env: &Env) -> BrowserKit {
        let status = browser_kit::kit_status(layout, env);
        let problem = |problem: browser_kit::KitProblem| KitProblem {
            problem: problem.problem,
            fix: problem.fix,
        };
        BrowserKit {
            problems: status.problems().into_iter().map(problem).collect(),
            library_problem: status.library_problem().map(problem),
            libraries_checked: status.unresolved_libraries.is_some(),
            inspection: status.inspection,
            node_fix: browser_kit::NODE_FIX,
        }
    }

    fn find_in_path(&self, program: &str, path: &str) -> Option<PathBuf> {
        browser_kit::find_in_path_value(program, path)
    }

    fn task_store_partitions(
        &self,
        global_root: &Path,
    ) -> Result<Option<TaskStorePartitions>, OrbitError> {
        let partition = |partition: UnclaimedPartition| TaskStorePartition {
            path: partition.path,
            task_bundles: partition.task_bundles,
        };
        Ok(
            inspect_task_store_partitions(global_root)?.map(|partitions| TaskStorePartitions {
                scanned: partitions.scanned,
                removable: partitions.removable.into_iter().map(partition).collect(),
                stale: partitions.stale.into_iter().map(partition).collect(),
                unowned: partitions.unowned.into_iter().map(partition).collect(),
                unreachable: partitions
                    .unreachable
                    .into_iter()
                    .map(|unreachable| UnreachableTaskStorePartition {
                        partition: partition(unreachable.partition),
                        reason: unreachable.reason,
                    })
                    .collect(),
            }),
        )
    }

    fn remove_unclaimed_task_stores(
        &self,
        global_root: &Path,
    ) -> Result<OrphanTaskStoreRemoval, OrbitError> {
        let removed = remove_unclaimed_task_stores(global_root)?;
        Ok(OrphanTaskStoreRemoval {
            empty_partitions: removed.empty.len(),
            populated_partitions: removed.stale.len(),
            task_bundles: removed.task_bundles_removed(),
        })
    }
}

/// The doctor's host reads, as a surface passes them to `orbit doctor`.
pub fn doctor_host() -> Arc<dyn Host> {
    Arc::new(DoctorHost)
}
