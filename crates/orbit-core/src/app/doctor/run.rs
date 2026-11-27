//! The doctor commands' flows: the repairs and the report, the admission
//! receipt storage, the provider rows' facts and the policy verdicts.

use std::time::Instant;

use orbit_common::{Env, OrbitError};
use orbit_types::policy::{DEFAULT_POLICY_NAME, FsOperation};

use super::report::{provider_limit_findings, run_doctor_report};
use super::{DoctorCommands, Host, Outcome, Providers, Repaired, Repairs};
use crate::Core;

/// One `orbit doctor` run's inputs.
pub(super) struct Report<'a, 'r> {
    pub(super) repairs: Repairs,
    pub(super) confirm: bool,
    pub(super) deep: bool,
    pub(super) env: &'a Env,
    pub(super) host: &'a dyn Host,
    pub(super) repaired: &'r mut dyn FnMut(Repaired, Instant),
}

/// Run the requested repairs in order, reporting each as it finishes, then
/// every read-only check. The first repair that fails ends the command.
pub(super) fn report(core: &Core, report: Report<'_, '_>) -> Result<Outcome, OrbitError> {
    let Report {
        repairs,
        confirm,
        deep,
        env,
        host,
        repaired,
    } = report;
    if repairs.orphan_task_stores && !confirm {
        return Err(OrbitError::InvalidInput(
            "--fix-orphan-task-stores deletes task-store partition directories and their task bundles. Pass --confirm to proceed."
                .to_string(),
        ));
    }
    let runtime = core.runtime();
    let cpu_source = runtime.resource_throttle_settings().cpu_source.to_string();
    let mut run = |requested: bool,
                   repair: &dyn Fn() -> Result<Repaired, OrbitError>|
     -> Result<(), OrbitError> {
        if requested {
            let started = Instant::now();
            repaired(repair()?, started);
        }
        Ok(())
    };
    run(repairs.missing_pr_refs, &|| {
        runtime
            .repair_missing_handoff_pr_refs()
            .map(Repaired::MissingPrRefs)
    })?;
    run(repairs.state_directory_permissions, &|| {
        runtime
            .repair_state_directory_permissions()
            .map(Repaired::StateDirectoryPermissions)
    })?;
    run(repairs.stale_locks, &|| {
        runtime.remove_stale_lock_files().map(Repaired::StaleLocks)
    })?;
    run(repairs.stale_task_locks, &|| {
        runtime
            .clear_stale_task_reservations()
            .map(Repaired::StaleTaskLocks)
    })?;
    run(repairs.graph, &|| {
        runtime.remove_retired_graph_state().map(Repaired::Graph)
    })?;
    run(repairs.stale_artifacts, &|| {
        runtime
            .remove_stale_definition_artifacts()
            .map(Repaired::StaleArtifacts)
    })?;
    run(repairs.orphan_task_stores, &|| {
        runtime
            .remove_orphan_task_stores(host)
            .map(Repaired::OrphanTaskStores)
    })?;
    run(repairs.automation_pins, &|| {
        crate::application::automation::release_unreferenced_attempt_pins(runtime)
            .map(Repaired::AutomationPins)
    })?;
    run(repairs.retired_config_keys, &|| {
        runtime
            .remove_retired_config_keys()
            .map(Repaired::RetiredConfigKeys)
    })?;
    Ok(Outcome::Report {
        cpu_source,
        rows: run_doctor_report(runtime, env, deep, host),
    })
}

/// The owner's admission receipt storage, compacted first when asked.
pub(super) fn admission_receipts(core: &Core, compact: bool) -> Result<Outcome, OrbitError> {
    let runtime = core.runtime();
    let (compacted_receipts, storage) = if compact {
        let report = runtime.compact_admission_receipts()?;
        (report.compacted_receipts, report.storage)
    } else {
        (0, runtime.inspect_admission_receipt_storage()?)
    };
    Ok(Outcome::AdmissionReceipts {
        compacted_receipts,
        storage,
    })
}

/// Each executor definition with the launcher dispatch would find, one
/// fresh Bubblewrap probe when any uses it, and the provider-limit rows.
pub(super) fn providers(core: &Core) -> Result<Providers, OrbitError> {
    let runtime = core.runtime();
    let defs = runtime.list_executor_defs()?;
    // One fresh user-scoped probe for this diagnostic. The configured executor
    // mode alone does not establish that the host can create the namespace.
    let linux_probe = defs
        .iter()
        .any(|def| {
            def.sandbox
                .is_some_and(|kind| kind.as_str() == "linux-bwrap")
        })
        .then(crate::bootstrap::linux_sandbox_host::probe_bwrap_fresh);
    // [ORB-14698] The same provider-limit rows `orbit doctor` reports, under
    // the executor of the provider each concerns.
    let limit_findings = provider_limit_findings(runtime);
    let executors = defs
        .into_iter()
        .map(|def| {
            let launcher = def
                .command
                .as_deref()
                .and_then(|program| runtime.locate_provider_launcher(program));
            (def, launcher)
        })
        .collect();
    Ok(Providers {
        executors,
        linux_probe,
        limit_findings,
    })
}

/// The default policy's read and modify verdicts for `path` under `profile`.
pub(super) fn fs_access(core: &Core, profile: &str, path: &str) -> Result<Outcome, OrbitError> {
    let def = core
        .runtime()
        .get_policy_def(DEFAULT_POLICY_NAME)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!("policy not found: {DEFAULT_POLICY_NAME}"))
        })?;
    let read = def.check_path(profile, FsOperation::Read, path)?;
    let modify = def.check_path(profile, FsOperation::Modify, path)?;
    Ok(Outcome::FsAccess { read, modify })
}
