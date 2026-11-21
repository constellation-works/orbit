use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use orbit_automation::routines::sweep::{TICK_DEADLINE, run_sweep_core};
use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::{self, LogRotationConfig};
use orbit_types::workspace::Workspace;

use super::super::RoutineMachineIdentity;
use super::super::loader::{RoutineLoadError, RoutineWorkspaceProvider, collect_host_routines};
use super::dispatch::RuntimeDispatch;
use super::{
    AutoTaskSweepReport, ExternalMergeSweepReport, RoutineSweepReport, SweepOptions, SweepOutcome,
    workspaces,
};
use crate::application::auto_tasks::SchedulerOptions;
use crate::application::auto_tasks::scheduler::run_auto_task_scheduler_with_cache;
use crate::application::automation::SourceCache;
use crate::runtime::host_signal::{HostSignalProbe, default_host_signal_probe};

/// Run one sweep pass against the default global root with a caller-supplied
/// workspace provider.
pub fn run_sweep_with_providers(
    options: SweepOptions,
    local_machine: RoutineMachineIdentity,
    workspace_provider: &dyn RoutineWorkspaceProvider,
) -> Result<SweepOutcome, OrbitError> {
    let global_root = crate::runtime::resolve_global_root()?;
    // The OS clock invokes this every minute forever; on macOS launchd
    // redirects stdout/stderr into `logs/sweep.log`. Opportunistically roll +
    // prune it here (rename-based, best-effort) so an always-on host cannot
    // grow it without bound. No-op until the file exceeds the
    // configured per-file budget. `run_sweep_at_with_providers` (the explicit
    // root seam) is left
    // untouched so tests never rotate real logs.
    log_rotation::rotate_and_prune(
        &super::super::clock::sweep_log_path(&global_root),
        &LogRotationConfig::default(),
    );
    run_sweep_at_with_providers(&global_root, options, local_machine, workspace_provider)
}

/// Run one sweep pass against an explicit global root using injected
/// composition. Provider calls occur only after the sweep lock is held.
pub fn run_sweep_at_with_providers(
    global_root: &Path,
    options: SweepOptions,
    local_machine: RoutineMachineIdentity,
    workspace_provider: &dyn RoutineWorkspaceProvider,
) -> Result<SweepOutcome, OrbitError> {
    run_sweep_at_with_providers_at(
        global_root,
        options,
        local_machine,
        workspace_provider,
        default_host_signal_probe().as_ref(),
        Utc::now(),
    )
}

/// Test seam for one scheduler tick at an explicit instant, with an injected
/// host-signal probe.
pub(crate) fn run_sweep_at_with_providers_at(
    global_root: &Path,
    options: SweepOptions,
    local_machine: RoutineMachineIdentity,
    workspace_provider: &dyn RoutineWorkspaceProvider,
    host_signals: &dyn HostSignalProbe,
    now_utc: chrono::DateTime<Utc>,
) -> Result<SweepOutcome, OrbitError> {
    let deadline = options
        .deadline
        .unwrap_or_else(|| Instant::now() + TICK_DEADLINE);
    let options = SweepOptions {
        deadline: Some(deadline),
        ..options
    };
    // One pass per host at a time: overlapping invocations from a slow prior
    // pass must not double-fire. flock releases on process death, so a
    // crashed sweep never wedges the next one. `lock_busy` means a pass that
    // recorded itself as the holder is in flight — not merely that the OS
    // refused the lock, which a descriptor inherited by a child this process
    // forked can also do [ORB-12532].
    let lock = orbit_store::try_acquire_routine_sweep_lock(&global_root.join("state"))?;
    let Some(_lock) = lock else {
        return Ok(SweepOutcome {
            machine_name: local_machine.machine_name,
            machine_id: local_machine.machine_id,
            lock_busy: true,
            ..SweepOutcome::default()
        });
    };

    let store = super::super::open_routine_store(global_root)?;
    // The OS unit and due calculation share this host-local setting. A
    // configured five-minute clock therefore keeps a slot natural for two
    // five-minute intervals instead of retaining the old 120-second default.
    let options = workspaces::configured_sweep_options(global_root, options)?;
    // One runtime per active workspace; discovery and dispatch share them.
    let mut discovered = workspace_provider.discover_workspaces_until(global_root, deadline)?;
    let mut skipped_workspaces: BTreeSet<String> =
        discovered.skipped_workspaces.iter().cloned().collect();
    let mut load_errors: Vec<RoutineLoadError> = discovered.errors.clone();
    let no_workspace_loaded = workspaces::no_workspace_loaded_row(&discovered);

    // [ORB-14173] [ORB-16354] Replica checkouts contribute only their
    // host-local worktree GC and store GC routines; the rest are reported
    // as owner work below.
    let host = if Instant::now() < deadline {
        collect_host_routines(&discovered)
    } else {
        skipped_workspaces.extend(
            discovered
                .entries
                .iter()
                .chain(&discovered.replicas)
                .map(|(workspace, _)| workspace.name.clone()),
        );
        super::super::loader::HostRoutines {
            collection: super::super::loader::RoutineCollection::default(),
            owner_only: Vec::new(),
        }
    };
    let mut collection = host.collection;
    load_errors.append(&mut collection.errors);

    // [ORB-13892] A follower's recorded pull settlements are retried here,
    // so one that missed its owner — a forced release while the owner was
    // down, a leaf whose worker died — reaches it without a new drain once
    // the leaf's worker and its drain are gone. Delivery starts nothing, so
    // a pending host shutdown does not hold it.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter().chain(&discovered.replicas),
            deadline,
            &mut skipped_workspaces,
            None,
            None,
            |workspace, runtime| {
                workspaces::deliver_recorded_pull_settlements(workspace, runtime);
                Ok(())
            },
        );
    }

    // Lifetime maintenance starts no work and must also run during an
    // admission hold. Load the clock credentials before checking the forge.
    let clock_env_loaded = if options.dry_run {
        Vec::new()
    } else {
        let loaded = workspaces::load_clock_env_for(global_root, &mut discovered, &mut load_errors);
        super::super::leases::sweep_owner_lease_lifetimes(
            &discovered,
            deadline,
            &mut skipped_workspaces,
        );
        loaded
    };

    // Settle owner tasks whose delivery PR merged outside Orbit. Runs after
    // the clock environment loads so forge credentials are available, and
    // before the host-shutdown hold: settlement starts no work. Only owner
    // entries settle; replicas are followers. A forge or store failure logs
    // and skips that workspace without failing routines or auto-tasks.
    let mut external_merge_settlements: Vec<ExternalMergeSweepReport> = Vec::new();
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.external_merge_settlement_failed"),
            None,
            |workspace, runtime| {
                let settled = runtime.settle_externally_merged_tasks()?;
                external_merge_settlements.extend(settled.into_iter().map(|settlement| {
                    ExternalMergeSweepReport {
                        task_id: settlement.task_id,
                        source: workspace.name.clone(),
                        pr_number: settlement.pr_number,
                    }
                }));
                Ok(())
            },
        );
    }

    // [ORB-25966] Merge and settle the delivered pull requests whose
    // authorised delivery ended with nobody left to finish them. Runs after
    // settlement so a merge made elsewhere settles first. Owner entries only.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.owner_auto_landing_failed"),
            None,
            |workspace, runtime| {
                for landed in runtime.land_completion_authorized_tasks()? {
                    tracing::info!(
                        target: "orbit_core::application::routines::sweep",
                        workspace = %workspace.name,
                        task_id = %landed.task_id,
                        pull_request = landed.pull_request,
                        "sweep.owner_auto_landed"
                    );
                }
                Ok(())
            },
        );
    }

    // Fleet notices linked to a done task in another workspace this host
    // owns are materialized closed here; reads already report them closed,
    // so this only keeps the store honest. Like settlement it starts no
    // work, so it also runs before the host-shutdown hold. Owner entries
    // only.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.notice_closure_failed"),
            None,
            |_, runtime| {
                if runtime.coordination_write_owner().is_none() {
                    runtime.run_notice_closure_tick()?;
                }
                Ok(())
            },
        );
    }

    // Resident mandates whose deadline has passed end here, on the owner,
    // whether or not their cycle routine runs: expiry starts no work, so
    // like notice closure it runs before the host-shutdown hold. Owner
    // entries only; the expiry itself also refuses a replica runtime.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.resident_deadline_expiry_failed"),
            None,
            |workspace, runtime| {
                let expired = runtime.expire_resident_mandates_past_deadline(now_utc)?;
                if !expired.is_empty() {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        expired = ?expired,
                        "sweep.resident_deadline_expired"
                    );
                }
                Ok(())
            },
        );
    }

    // [ORB-12968] A host shutdown inside the hold horizon would kill anything
    // this tick starts, so the whole fire phase — routines and auto-tasks —
    // stands down for it. A later schedule does not. Cursors are not advanced
    // while held, so once the schedule is cancelled, the horizon lifts, or
    // the host is back, each routine's own `missed_run` policy decides what
    // the held slots become, exactly as after any other downtime. The fixed
    // horizon is judged against this tick's clock.
    if let Some(shutdown) = workspaces::shutdown_holding_admission(host_signals, now_utc) {
        let reason = shutdown.hold_reason();
        tracing::warn!(
            target: "orbit.core.sweep",
            mode = shutdown.mode.as_str(),
            scheduled_at = %shutdown.scheduled_at,
            routines = collection.routines.len(),
            "sweep.host_shutdown_hold: {}; no routine or auto-task fires this tick",
            shutdown.describe(),
        );
        return Ok(SweepOutcome {
            machine_name: local_machine.machine_name,
            machine_id: local_machine.machine_id,
            lock_busy: false,
            reports: collection
                .routines
                .iter()
                .map(|routine| RoutineSweepReport {
                    routine: routine.definition.name.clone(),
                    source: routine.source_workspace.clone(),
                    origin: routine.origin.as_str(),
                    action: "skipped",
                    reason: Some(reason.clone()),
                    slot: None,
                    run_id: None,
                    batch: Vec::new(),
                })
                .collect(),
            auto_task_reports: Vec::new(),
            load_errors,
            clock_env_loaded,
            no_workspace_loaded,
            external_merge_settlements,
            deadline_exceeded: Instant::now() >= deadline,
            skipped_workspaces: skipped_workspaces.into_iter().collect(),
        });
    }

    // Only owners resume authorized landing work. Honor dry runs, the tick
    // deadline and shutdown admission above; replicas never dispatch landings.
    if !options.dry_run {
        for (workspace, runtime) in &discovered.entries {
            if Instant::now() >= deadline {
                skipped_workspaces.insert(workspace.name.clone());
                continue;
            }
            if let Err(error) = runtime.dispatch_landing_requests_at(now_utc) {
                tracing::warn!(
                    target: "orbit_core::application::routines::sweep",
                    workspace = %workspace.name,
                    %error,
                    "sweep.owner_landing_dispatch_failed"
                );
            }
        }
    }

    let dispatch = RuntimeDispatch {
        runtimes: discovered
            .entries
            .iter()
            .chain(&discovered.replicas)
            .map(|(_, runtime)| (runtime.shared_root(), runtime))
            .collect(),
        shown_runs: RefCell::new(HashMap::new()),
        source_cache: SourceCache::default(),
    };

    let mut reports = run_sweep_core(store.as_ref(), &collection, &dispatch, options, now_utc)?;
    skipped_workspaces.extend(
        reports
            .iter()
            .filter(|report| report.reason.as_deref() == Some("tick_deadline"))
            .map(|report| report.source.clone()),
    );
    // A definition targeting a retired job is skipped, not broken: one
    // non-noteworthy row per pass, never a load error on every tick.
    reports.extend(collection.retired.iter().map(|routine| RoutineSweepReport {
        routine: routine.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action: "retired",
        reason: Some(routine.reason.clone()),
        slot: None,
        run_id: None,
        batch: Vec::new(),
    }));
    reports.extend(host.owner_only.iter().map(|owned| RoutineSweepReport {
        routine: owned.routine.definition.name.clone(),
        source: owned.routine.source_workspace.clone(),
        origin: owned.routine.origin.as_str(),
        action: "skipped",
        reason: Some(format!("owner_only_in_replica: {}", owned.reason)),
        slot: None,
        run_id: None,
        batch: Vec::new(),
    }));
    // Auto-tasks run second so their task-store writes cannot delay routine
    // dispatch. Only owner checkouts mint: a replica holds no task store of
    // its own. The phase is bounded by this pass's discovered workspaces and
    // each scheduler's finite definition collection. A workspace-level error
    // becomes one row and never prevents the remaining workspaces from running.
    let auto_task_reports = RefCell::new(Vec::new());
    let mut on_auto_task_deadline = |workspace: &Workspace| {
        auto_task_reports.borrow_mut().push(AutoTaskSweepReport {
            name: "auto-tasks".into(),
            source: workspace.name.clone(),
            action: "skipped",
            reason: Some("tick_deadline".into()),
            slot: None,
            task_id: None,
        });
    };
    workspaces::each_workspace_in_tick(
        discovered.entries.iter(),
        deadline,
        &mut skipped_workspaces,
        None,
        Some(&mut on_auto_task_deadline),
        |workspace, runtime| {
            match run_auto_task_scheduler_with_cache(
                runtime,
                now_utc,
                SchedulerOptions {
                    dry_run: options.dry_run,
                },
                &dispatch.source_cache,
            ) {
                Ok(outcome) => {
                    let mut reports = auto_task_reports.borrow_mut();
                    reports.extend(
                        outcome
                            .reports
                            .into_iter()
                            .map(|report| AutoTaskSweepReport {
                                name: report.name,
                                source: workspace.name.clone(),
                                action: if report.action == "fired" {
                                    "minted"
                                } else {
                                    report.action
                                },
                                reason: report.reason,
                                slot: report.slot,
                                task_id: report.task_id,
                            }),
                    );
                    reports.extend(outcome.errors.into_iter().map(|error| {
                        AutoTaskSweepReport {
                            name: error
                                .path
                                .as_ref()
                                .and_then(|path| path.file_stem())
                                .and_then(|name| name.to_str())
                                .unwrap_or("auto-task")
                                .to_string(),
                            source: workspace.name.clone(),
                            action: "error",
                            reason: Some(error.message),
                            slot: None,
                            task_id: None,
                        }
                    }));
                }
                Err(error) => auto_task_reports.borrow_mut().push(AutoTaskSweepReport {
                    name: "auto-tasks".to_string(),
                    source: workspace.name.clone(),
                    action: "error",
                    reason: Some(error.to_string()),
                    slot: None,
                    task_id: None,
                }),
            }
            Ok(())
        },
    );
    let auto_task_reports = auto_task_reports.into_inner();
    // [ORB-14273] After the executable generation settles, the clock resumes each
    // run the current upgrade interrupted at most once, and audits every run it
    // skips (an earlier upgrade's, an elapsed or stopped drain, a superseded one).
    if !options.dry_run && orbit_common::fs::generation::pending_switch(global_root).is_none() {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.upgrade_interrupted_runs_resume_failed"),
            None,
            |workspace, runtime| {
                let resumed = runtime.auto_resume_upgrade_interrupted_runs(now_utc)?;
                if !resumed.is_empty() {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        resumed = resumed.len(),
                        "sweep.upgrade_interrupted_runs_resumed"
                    );
                }
                Ok(())
            },
        );
    }

    // [ORB-25991] Standing drain keeper: once the generation has settled and
    // after upgrade resume has had its turn, an owner whose
    // `workflow.standing_drain.enabled` is set and whose drain intent is `on`
    // gets one bounded drain when none is live and work waits. Off (the
    // default) reads and writes nothing. Owner entries only; replicas never
    // submit.
    if !options.dry_run && orbit_common::fs::generation::pending_switch(global_root).is_none() {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.standing_drain_keeper_failed"),
            None,
            |workspace, runtime| {
                if let Some(crate::application::job::KeeperDecision::Submitted { run_id }) =
                    runtime.run_standing_drain_keeper_tick(now_utc)?
                {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        run_id = %run_id,
                        "sweep.standing_drain_keeper_submitted"
                    );
                }
                Ok(())
            },
        );
    }

    // [ORB-14617] A delivery run held because the forge refused its push is
    // resumed from its checkpoints each tick, within its retry window; past
    // the window its tasks are blocked for a human.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.forge_held_runs_failed"),
            None,
            |workspace, runtime| {
                let tick = runtime.auto_resume_forge_held_runs(now_utc)?;
                if !tick.resumed.is_empty() || !tick.expired.is_empty() {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        resumed = tick.resumed.len(),
                        expired = tick.expired.len(),
                        "sweep.forge_held_runs"
                    );
                }
                Ok(())
            },
        );
    }

    // [ORB-20158] A job run parked on an auto-task step is revisited when
    // due: the same logical run continues, it is never submitted again.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.parked_runs_wake_failed"),
            None,
            |workspace, runtime| {
                let woken = runtime.wake_parked_job_runs(now_utc)?;
                if !woken.is_empty() {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        woken = woken.len(),
                        "sweep.parked_runs_woken"
                    );
                }
                Ok(())
            },
        );
    }

    // The final-recovery backstop runs last: it only submits runs for tasks
    // already blocked, and stands down itself off the owner or with an empty
    // `workflow.purpose_crews.final_recovery` pool.
    if !options.dry_run {
        {
            // Recovery must be followed by evidence fulfilment per workspace;
            // both steps recheck the same tick deadline independently.
            let mut workspace_is_allowed = |workspace: &Workspace| {
                workspaces::tick_allows_workspace(deadline, workspace, &mut skipped_workspaces)
            };
            for (workspace, runtime) in &discovered.entries {
                if !workspace_is_allowed(workspace) {
                    continue;
                }
                match runtime.run_blocked_task_recovery_tick(now_utc) {
                    Ok(tick) => {
                        if !tick.dispatched.is_empty()
                            || !tick.settled.is_empty()
                            || !tick.rechecked.is_empty()
                        {
                            tracing::info!(
                                target: "orbit.core.sweep",
                                workspace = %workspace.name,
                                dispatched = tick.dispatched.len(),
                                settled = tick.settled.len(),
                                rechecked = tick.rechecked.len(),
                                "sweep.blocked_task_recovery"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        error = %error,
                        "sweep.blocked_task_recovery_failed"
                    ),
                }
                // A held review whose only missing evidence is a Linux CodeQL
                // run gets that run here, on a Linux owner; one at a time.
                if !workspace_is_allowed(workspace) {
                    continue;
                }
                match runtime.run_review_evidence_fulfilment_tick(now_utc) {
                    Ok(tick) => {
                        if !tick.dispatched.is_empty() {
                            tracing::info!(
                                target: "orbit.core.sweep",
                                workspace = %workspace.name,
                                dispatched = tick.dispatched.len(),
                                "sweep.review_evidence_fulfilment"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        error = %error,
                        "sweep.review_evidence_fulfilment_failed"
                    ),
                }
            }
        }
    }

    // Persist unattended forecasts on the owner clock as well as canonical
    // reads. A recent hourly slot avoids history reads and simulation; all
    // failures stay local to this workspace and never fail the tick.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.backlog_forecast_failed"),
            None,
            |_, runtime| runtime.record_scheduled_backlog_forecast(now_utc),
        );
    }

    // [ORB-14739, ORB-14823] Standing baseline-red holds are re-checked
    // here, never on a read or admission path. The tick reads only recorded
    // base results: a base tip nobody has checked gets one detached refresh
    // run per workspace, so the required command never runs under the sweep
    // lock. Admission reads the recorded verdict.
    if !options.dry_run {
        workspaces::each_workspace_in_tick(
            discovered.entries.iter(),
            deadline,
            &mut skipped_workspaces,
            Some("sweep.baseline_holds_failed"),
            None,
            |workspace, runtime| {
                let refresh = runtime.run_baseline_hold_tick(deadline)?;
                if !refresh.lifted.is_empty()
                    || !refresh.held.is_empty()
                    || refresh.dispatched.is_some()
                    || !refresh.repairs_filed.is_empty()
                    || !refresh.repairs_linked.is_empty()
                {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        workspace = %workspace.name,
                        lifted = refresh.lifted.len(),
                        held = refresh.held.len(),
                        unchecked = refresh.unchecked.len(),
                        dispatched = refresh.dispatched.as_deref().unwrap_or_default(),
                        repairs_filed = refresh.repairs_filed.len(),
                        repairs_linked = refresh.repairs_linked.len(),
                        "sweep.baseline_holds"
                    );
                }
                Ok(())
            },
        );
    }

    let deadline_exceeded = Instant::now() >= deadline;
    if deadline_exceeded {
        tracing::error!(target: "orbit.core.sweep", skipped_workspaces = ?skipped_workspaces,
            "sweep.tick_deadline: tick budget exhausted; remaining work waits for the next tick");
    }
    Ok(SweepOutcome {
        machine_name: local_machine.machine_name,
        machine_id: local_machine.machine_id,
        lock_busy: false,
        reports,
        auto_task_reports,
        load_errors,
        clock_env_loaded,
        no_workspace_loaded,
        external_merge_settlements,
        deadline_exceeded,
        skipped_workspaces: skipped_workspaces.into_iter().collect(),
    })
}
