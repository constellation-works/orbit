//! Work that cannot progress: `running`/`pending` job runs with no live worker,
//! and delivery automation consumers that are stalled, wedged, unresolvable or
//! unadmittable on this host.

use crate::doctor::*;

/// `running`/`pending` job runs with no live worker process — dead recorded
/// owner, or a queued run no worker ever claimed (read-only view of the
/// reconcile signal; see `job/run/reconcile.rs`) [ORB-10070].
pub(in crate::doctor) fn doctor_check_job_runs(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let running = match runtime.list_orphaned_running_job_runs() {
        Ok(orphans) => orphans,
        Err(error) => {
            return check(
                "job-runs",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect job runs: {error}"),
            );
        }
    };
    let pending = match runtime.list_orphaned_pending_job_runs() {
        Ok(orphans) => orphans,
        Err(error) => {
            return check(
                "job-runs",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect job runs: {error}"),
            );
        }
    };
    if running.is_empty() && pending.is_empty() {
        return check(
            "job-runs",
            WorkspaceDoctorStatus::Ok,
            "no orphaned running or pending job runs".to_string(),
        );
    }
    let mut segments = Vec::new();
    if !running.is_empty() {
        let ids: Vec<&str> = running.iter().map(|run| run.run_id.as_str()).collect();
        segments.push(format!(
            "{} running run(s) whose owner process is gone: {} — they \
             finalize as `interrupted` on reconcile; resume with \
             `orbit job resume <run_id>`",
            running.len(),
            ids.join(", ")
        ));
    }
    if !pending.is_empty() {
        let ids: Vec<&str> = pending.iter().map(|run| run.run_id.as_str()).collect();
        segments.push(format!(
            "{} pending run(s) with no live worker process: {} — they \
             finalize as `interrupted` on reconcile; clear immediately with \
             `orbit run cancel <run_id>`",
            pending.len(),
            ids.join(", ")
        ));
    }
    actionable_check(
        "job-runs",
        WorkspaceDoctorStatus::Warning,
        segments.join("; "),
        "For each named running run use `orbit job resume <run_id>`; for each named pending run use `orbit run cancel <run_id>`.".to_string(),
    )
}

/// Delivery automation consumers that cannot make progress: evaluation
/// suspended by a stall, an enabled definition whose branch does not exist, or
/// an enabled definition this host may never admit work for.
///
/// A stalled consumer is silent by design — it stops reporting a per-tick
/// error precisely so the debt is visible here instead of scrolling past in a
/// sweep log. Nothing resumes it without an operator, so it is reported until
/// one recovers or resets it.
///
/// A definition whose configured branch git cannot resolve never baselines,
/// so no stall marker ever exists for it; every sweep would defer with the
/// same reason forever. Doctor names the branch, git's text and the fix.
///
/// An enabled definition whose resolved owner is another machine — or whose
/// ownership resolves to nobody — is refused at every tick and accumulates
/// coverage debt it can never discharge [ORB-12867]. It carries no stall
/// marker and its branch may resolve perfectly, so nothing else here would
/// notice. A disabled definition stays quiet: `disabled` already says why.
///
/// A consumer whose admitted action stopped without acceptable evidence — its
/// task closed with missing or malformed coverage — is wedged: evaluation
/// settles it, so one still reported means no evaluation reaches it, and it
/// admits nothing new until one does or an operator recovers or resets it.
pub(in crate::doctor) fn doctor_check_stalled_automation(
    runtime: &OrbitRuntime,
) -> WorkspaceDoctorResult {
    let stalled = match orbit_core::application::automation::stalled_consumers(runtime) {
        Ok(stalled) => stalled,
        Err(error) => {
            return check(
                "automation-consumers",
                WorkspaceDoctorStatus::Warning,
                format!("cannot read delivery automation state: {error}"),
            );
        }
    };
    let unresolvable =
        match orbit_core::application::automation::unresolvable_delivery_branches(runtime) {
            Ok(unresolvable) => unresolvable,
            Err(error) => {
                return check(
                    "automation-consumers",
                    WorkspaceDoctorStatus::Warning,
                    format!("cannot read delivery automation definitions: {error}"),
                );
            }
        };

    let unadmittable =
        match orbit_core::application::automation::unadmittable_delivery_definitions(runtime) {
            Ok(unadmittable) => unadmittable,
            Err(error) => {
                return check(
                    "automation-consumers",
                    WorkspaceDoctorStatus::Warning,
                    format!("cannot resolve delivery automation ownership: {error}"),
                );
            }
        };

    let now = chrono::Utc::now();
    let wedged = match orbit_core::application::automation::wedged_delivery_consumers(runtime, now)
    {
        Ok(wedged) => wedged,
        Err(error) => {
            return check(
                "automation-consumers",
                WorkspaceDoctorStatus::Warning,
                format!("cannot read delivery automation actions: {error}"),
            );
        }
    };

    if stalled.is_empty() && unresolvable.is_empty() && unadmittable.is_empty() && wedged.is_empty()
    {
        return check(
            "automation-consumers",
            WorkspaceDoctorStatus::Ok,
            "no stalled delivery automation consumer".to_string(),
        );
    }

    let mut segments = Vec::new();
    let mut remediation = Vec::new();
    if !wedged.is_empty() {
        let detail = wedged
            .iter()
            .map(|consumer| match &consumer.reason {
                Some(reason) => format!(
                    "{} (action {}: {reason})",
                    consumer.definition, consumer.action_id
                ),
                None => format!("{} (action {})", consumer.definition, consumer.action_id),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let first = wedged
            .first()
            .map(|consumer| consumer.definition.clone())
            .unwrap_or_default();
        segments.push(format!(
            "{} delivery automation consumer(s) wedged on an action that closed without \
             accepted coverage evidence: {detail}",
            wedged.len()
        ));
        remediation.push(format!(
            "The next delivery evaluation on the owning host settles the action and applies \
             the retry budget. Without one, `orbit auto-task recover {first} --reissue-action \
             --reason <why>` re-examines the same batch, or `orbit auto-task reset {first} \
             --reason <why>` forgets its debt."
        ));
    }
    if !stalled.is_empty() {
        let detail = stalled
            .iter()
            .map(|consumer| {
                format!(
                    "{} ({}, stalled {} min)",
                    consumer.definition(),
                    consumer.stall.reason,
                    orbit_core::application::automation::stalled_minutes(&consumer.stall, now)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let first = stalled
            .first()
            .map(|consumer| consumer.definition().to_string())
            .unwrap_or_default();
        segments.push(format!(
            "{} delivery automation consumer(s) stalled and accumulating debt: {detail}",
            stalled.len()
        ));
        remediation.push(format!(
            "Inspect with `orbit auto-task recover {first}`, then either \
             `orbit auto-task recover {first} --replay-history --reason <why>` to retain \
             the debt, or `orbit auto-task reset {first} --reason <why>` to forget it."
        ));
    }
    if !unresolvable.is_empty() {
        let detail = unresolvable
            .iter()
            .map(|consumer| {
                format!(
                    "{} targets `{}`: {}",
                    consumer.definition, consumer.branch, consumer.error
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let base_branch = runtime.workspace_base_branch();
        segments.push(format!(
            "{} enabled delivery definition(s) name a branch that does not exist in this \
             repository and can never baseline: {detail}",
            unresolvable.len()
        ));
        remediation.push(format!(
            "For each named definition set `schedule.deliveries_landed.branch` to the \
             workspace base branch `{base_branch}` (edit the file under `.orbit/auto_tasks/` \
             or pass `--deliveries-landed` to `orbit auto-task update <name>`), or create \
             the branch it names; then rerun `orbit doctor`."
        ));
    }

    if !unadmittable.is_empty() {
        let detail = unadmittable
            .iter()
            .map(|definition| format!("{} ({})", definition.definition, definition.mismatch()))
            .collect::<Vec<_>>()
            .join("; ");
        segments.push(format!(
            "{} enabled delivery definition(s) this host can never admit work for, so their \
             coverage debt only grows: {detail}",
            unadmittable.len()
        ));
        if let Some(first) = unadmittable.first() {
            remediation.push(format!(
                "`{}` is refused because {}; `orbit auto-task show {} --preview` reports the \
                 coverage debt it is holding. For each named definition either make this host \
                 the resolved owner (set `schedule.deliveries_landed.owner_machine`, or \
                 register this workspace's owner machine when none resolves) or disable it \
                 here with `orbit auto-task toggle {} off`.",
                first.definition,
                first.mismatch(),
                first.definition,
                first.definition,
            ));
        }
    }

    actionable_check(
        "automation-consumers",
        WorkspaceDoctorStatus::Warning,
        segments.join("; "),
        remediation.join(" "),
    )
}
