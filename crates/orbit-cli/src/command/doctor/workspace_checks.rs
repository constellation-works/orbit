use orbit_common::Env;
use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::doctor::{
    Command, Outcome, Repaired, Repairs, WorkspaceDoctorResult, WorkspaceDoctorStatus, exec,
    misplaced_plugin_enablement_row,
};

use super::command::DoctorCommand;
use crate::command::CommandOut;
use crate::output::doctor::DoctorReportFilter;

impl DoctorCommand {
    fn report_filter(&self) -> DoctorReportFilter {
        if self.problems {
            DoctorReportFilter::ProblemsOnly
        } else {
            DoctorReportFilter::AllChecks
        }
    }

    /// The report `orbit doctor` can still give when the runtime would not
    /// open, or `None` to let the bootstrap error stand.
    ///
    /// Only a plain health check qualifies: a focused subcommand or a repair
    /// flag needs the runtime it asked for. The plain check opens the
    /// runtime write-free, so a workspace whose stores were never created or
    /// whose stores need a migration fails that open; the report then names
    /// the setup step instead of the bare storage error (STD-01 §R31). Stores
    /// that exist but whose open refusal says a writable open is still owed
    /// are the migration case. A store this binary cannot read (an
    /// unsupported schema, which a writable open cannot migrate) and any other
    /// bootstrap failure with both stores present still fail closed with the
    /// store's own upgrade or recovery guidance. A misplaced
    /// global `[plugin_enablement]` table, read straight from the host config
    /// because that table is what makes the layered config refuse to load, is
    /// reported alongside.
    pub(crate) fn degraded_report(
        &self,
        root_override: Option<&std::path::Path>,
        bootstrap: &OrbitError,
    ) -> Option<CommandOut> {
        if self.command.is_some() || self.repairs.any_requested() {
            return None;
        }
        let cwd = std::env::current_dir().ok()?;
        let global_root =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::resolve_roots_for_cwd(
                &cwd,
                root_override,
            )
            .ok()?
            .global_root;
        let misplaced = misplaced_plugin_enablement_row(&global_root);
        let missing_store = !global_root.join("orbit.db").exists()
            || !global_root.join("tasks").join("index.sqlite").exists();
        let needs_migration = !missing_store && bootstrap.owes_writable_store_open();
        if misplaced.is_none() && !missing_store && !needs_migration {
            return None;
        }
        let remediation = if missing_store {
            "Run `orbit task list` once to create the workspace stores, then rerun `orbit doctor`."
        } else if needs_migration {
            "Run `orbit task list` once to migrate the workspace stores, then rerun `orbit doctor`."
        } else {
            "Fix the check above, then rerun `orbit doctor` for the full report."
        };
        let runtime = WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "runtime".to_string(),
            status: WorkspaceDoctorStatus::Error,
            message: format!("The runtime did not open, so no other check ran: {bootstrap}"),
            remediation: Some(remediation.to_string()),
        };
        Some(crate::output::doctor::report_output(
            misplaced.into_iter().chain([runtime]).collect(),
            self.report_filter(),
            self.check.as_deref(),
        ))
    }

    /// The bare `orbit doctor` body: optional repairs, then the workspace report.
    pub(super) fn workspace_checks(self, core: &Core, env: &Env) -> CommandOut {
        let flags = &self.repairs;
        let repairs = Repairs {
            missing_pr_refs: flags.fix_missing_pr_refs,
            state_directory_permissions: flags.fix_state_directory_permissions,
            stale_locks: flags.fix_stale_locks,
            stale_task_locks: flags.fix_stale_task_locks,
            graph: flags.remove_graph,
            stale_artifacts: flags.fix_stale_artifacts,
            orphan_task_stores: flags.fix_orphan_task_stores,
            automation_pins: flags.fix_automation_pins,
            retired_config_keys: flags.fix_retired_config_keys,
        };
        let mut repaired_rows = Vec::new();
        let mut repaired = |repair: Repaired, started: std::time::Instant| {
            crate::output::doctor::push_repaired(&mut repaired_rows, repair, started);
        };
        let Outcome::Report { cpu_source, rows } = exec(
            core,
            Command::Report {
                repairs,
                confirm: self.confirm,
                deep: self.deep,
                env: env.clone(),
                host: orbit_cmd::doctor_host(),
                repaired: &mut repaired,
            },
        )?
        else {
            unreachable!("a doctor report command answers with its rows")
        };
        let mut results = vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "resource-throttle".to_string(),
            status: WorkspaceDoctorStatus::Info,
            message: format!("CPU source: {cpu_source}"),
            remediation: None,
        }];
        results.extend(repaired_rows);
        results.extend(rows);
        crate::output::doctor::report_output(results, self.report_filter(), self.check.as_deref())
    }
}
