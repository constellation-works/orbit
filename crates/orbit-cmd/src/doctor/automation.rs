use std::collections::BTreeSet;

use orbit_core::application::distributed::PendingPullSettlements;
use orbit_core::application::health::artifact::ArtifactCondition;

use super::*;

/// `running`/`pending` job runs with no live worker process — dead recorded
/// owner, or a queued run no worker ever claimed (read-only view of the
/// reconcile signal; see `job/run/reconcile.rs`) [ORB-10070].
pub(super) fn doctor_check_job_runs(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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

/// The latest pull window remains actionable after it ends on wire skew.
/// A newer healthy window supersedes that diagnosis, avoiding stale warnings.
pub(super) fn doctor_check_pull_protocol(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "pull-protocol";
    let inspect = || -> Result<Option<String>, OrbitError> {
        let runs = runtime.list_job_runs(orbit_core::application::job::JobRunListParams {
            job_id: Some(orbit_core::application::distributed::PULL_DRAIN_JOB.to_string()),
            limit: Some(1),
            ..Default::default()
        })?;
        let Some(run) = runs.first() else {
            return Ok(None);
        };
        let state = runtime.read_run_state(&run.run_id)?;
        let pass = state
            .as_ref()
            .and_then(|state| state.drain_last_pass.as_ref());
        let failure = run
            .steps
            .iter()
            .find(|step| step.error_code.as_deref() == Some("protocol_skew"));
        if failure.is_some()
            || pass
                .is_some_and(|pass| pass.last_pass_error_code.as_deref() == Some("protocol_skew"))
        {
            let detail = failure
                .and_then(|step| step.error_message.as_deref())
                .or_else(|| pass.and_then(|pass| pass.last_pass_error.as_deref()))
                .unwrap_or("pull request schema mismatch");
            return Ok(Some(format!(
                "{} ended {} with protocol_skew: {detail}",
                run.run_id, run.state
            )));
        }
        Ok(None)
    };
    match inspect() {
        Ok(Some(message)) => actionable_check(CHECK, WorkspaceDoctorStatus::Warning, message,
            "Deploy matching Orbit builds on the owner and follower, restart their long-lived processes, then start a new pull drain.".to_string()),
        Ok(None) => check(CHECK, WorkspaceDoctorStatus::Ok, "no protocol skew recorded on the latest pull drain".to_string()),
        Err(error) => check(CHECK, WorkspaceDoctorStatus::Warning, format!("cannot inspect pull protocol: {error}")),
    }
}

/// Follower pull settlements recorded locally but never delivered to the
/// owner. Nothing retries delivery on a timer, so the owner keeps the claim
/// `running` until an operator runs a settle-only pass; this row is what makes
/// that wait visible. A workspace that never pulled reports none.
pub(super) fn doctor_check_pull_settlements(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    pull_settlements_finding(runtime.pending_pull_settlements(), chrono::Utc::now())
}

pub(super) fn pull_settlements_finding(
    pending: Result<PendingPullSettlements, OrbitError>,
    now: chrono::DateTime<chrono::Utc>,
) -> WorkspaceDoctorResult {
    const CHECK: &str = "pull-settlements";
    let pending = match pending {
        Ok(pending) => pending,
        Err(error) => {
            return check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect pull settlements: {error}"),
            );
        }
    };
    if pending.count == 0 {
        return check(
            CHECK,
            WorkspaceDoctorStatus::Ok,
            "no pull settlements are waiting to be delivered to an owner".to_string(),
        );
    }
    let oldest = pending
        .oldest_age(now)
        .map(|age| format!(", the oldest waiting {}", format_age(age)))
        .unwrap_or_default();
    actionable_check(
        CHECK,
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} pull settlement(s) recorded here but not delivered to the owner{oldest}; the \
             owner still shows those claims as running and nothing retries on a timer",
            pending.count
        ),
        "Run `orbit run auto --stop` once the owner is reachable to deliver them, then rerun \
         `orbit doctor`."
            .to_string(),
    )
}

/// Coarse age for a diagnostic line: the two most significant units.
fn format_age(age: chrono::Duration) -> String {
    let seconds = age.num_seconds().max(0);
    let (days, hours, minutes) = (
        seconds / 86_400,
        seconds % 86_400 / 3_600,
        seconds % 3_600 / 60,
    );
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

/// A pending host shutdown or reboot holds every unattended admission —
/// routine and auto-task fires, drain waves, the ship sweep — so doctor names
/// it with its mode and time [ORB-12968]. Nothing is wrong with the workspace;
/// the warning explains why no new work is starting.
pub(super) fn doctor_check_host_shutdown(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    match runtime.scheduled_host_shutdown() {
        None => check(
            "host-shutdown",
            WorkspaceDoctorStatus::Ok,
            "no host shutdown or reboot is scheduled".to_string(),
        ),
        Some(shutdown) => actionable_check(
            "host-shutdown",
            WorkspaceDoctorStatus::Warning,
            format!(
                "{} (read from {}); scheduled routines, auto-tasks, drain waves and the ship \
                 sweep start no new runs until it clears, and in-flight runs are not touched",
                shutdown.describe(),
                shutdown.source
            ),
            "Nothing to repair: admissions resume on their own after the restart. To resume \
             them now, cancel the schedule as root with `shutdown -c`."
                .to_string(),
        ),
    }
}

/// `execution.env.pass` variables this process does not hold [ORB-14777].
///
/// Agents inherit the launching environment through the allowlist, so a
/// pass-listed variable missing here (a dedicated worker token, say) silently
/// leaves agents on another login. A warning, never an error: optional
/// provider keys can legitimately be absent. Names only, never values.
pub(super) fn doctor_check_env_pass(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let unset = runtime.unset_env_pass_names();
    if unset.is_empty() {
        return check(
            "env-pass",
            WorkspaceDoctorStatus::Ok,
            "every operator-added `execution.env.pass` variable is set in this environment"
                .to_string(),
        );
    }
    actionable_check(
        "env-pass",
        WorkspaceDoctorStatus::Warning,
        format!(
            "`execution.env.pass` names variables unset or empty in this environment: {}; \
             agents started from it will not receive them",
            unset.join(", ")
        ),
        "Export the variables before starting the drain or service. Drains inherit the \
         launching shell's environment, so start them from a login shell (or set the \
         variables in the unit's environment), or remove the names from \
         `execution.env.pass`, then rerun `orbit doctor`."
            .to_string(),
    )
}

/// Where required validation finds the user's toolchain [ORB-13987].
///
/// Required commands run with PATH and toolchain locators resolved from the
/// owner's login shell, independent of whatever launches the worker. This row
/// resolves that environment the way a delivery run would and warns when the
/// login shell cannot be probed (validation then falls back to the launcher's
/// PATH), when resolution is disabled without a configured PATH, or when the
/// resolved PATH drops login-shell entries. It also reports the successful
/// probe mode, interactive fallback reason and tool locations, warning when a
/// later PATH entry contains a different executable shadowed by an earlier one.
pub(super) fn doctor_check_validation_env(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    if runtime.workflow_required_validation_commands().is_empty() {
        return check(
            "validation-env",
            WorkspaceDoctorStatus::Skipped,
            "no `workflow.required_validation_commands`; required validation does not run here"
                .to_string(),
        );
    }
    let environment = runtime.validation_environment();
    let mut details = vec![format!(
        "required validation PATH comes from {}{}; probe mode: {}; PATH={}",
        environment.source.as_str(),
        environment
            .login_shell
            .as_ref()
            .map(|shell| format!(" ({})", shell.display()))
            .unwrap_or_default(),
        environment.probe_mode.map_or(
            if environment.login_shell_enabled {
                "failed"
            } else {
                "disabled"
            },
            |mode| mode.as_str()
        ),
        environment.path().unwrap_or("<unset>")
    )];
    let mut warning = false;
    if let Some(reason) = &environment.fallback_reason {
        details.push(format!("interactive fallback reason: {reason}"));
        warning = true;
    }
    if let Some(reason) = environment.preflight_warning() {
        details.push(reason);
        warning = true;
    }
    for tool in ["python3", "git", "make"] {
        let paths = environment.program_paths(tool);
        if let Some((first, later)) = paths.split_first() {
            details.push(format!("{tool} resolves to {}", first.display()));
            if !later.is_empty() {
                details.push(format!(
                    "{tool}: {} shadows later PATH executables: {}",
                    first.display(),
                    later
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                warning = true;
            }
        } else {
            details.push(format!("{tool} is absent from PATH"));
        }
    }
    let message = details.join("; ");
    if warning {
        actionable_check(
            "validation-env",
            WorkspaceDoctorStatus::Warning,
            message,
            "Make the interactive login shell export the intended toolchain PATH (`$SHELL \
             -i -l -c 'echo $PATH'` should list it first), fix rc failures or set \
             `workflow.validation_env.interactive = false` to use login profiles only, or \
             set `workflow.validation_env.path` with `orbit config set`, then rerun \
             `orbit doctor`."
                .to_string(),
        )
    } else {
        check("validation-env", WorkspaceDoctorStatus::Ok, message)
    }
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
pub(super) fn doctor_check_stalled_automation(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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

/// Both automatic-review switches in one row [ORB-13992]: before-PR review
/// (`review.before_pr`, its minutes and crew) and after-landing review (the
/// `delivery-code-review` auto-task, with when its next batch is due), each
/// with its source. A switch that is on but cannot run here reviews nothing
/// while every other surface looks healthy, so that is an error, not a
/// warning: before-PR review on a local-only ship workspace [ORB-14168],
/// before-PR review without a resolvable crew, or an after-landing
/// consumer that is missing, owned by another machine, wedged, stalled, held
/// for an operator, watching a branch that does not resolve, naming a crew
/// that does not, or trailing `origin/<branch>` past the batch's
/// `max_wait_minutes`. The trail is the remote-tracking ref already in the
/// checkout; this check does not fetch.
pub(super) fn doctor_check_review(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "review";
    let switches =
        match orbit_core::application::review::review_switches(runtime, chrono::Utc::now()) {
            Ok(switches) => switches,
            Err(error) => {
                return actionable_check(
                    CHECK,
                    WorkspaceDoctorStatus::Error,
                    format!("cannot establish which automatic review runs here: {error}"),
                    "Resolve the error, then rerun `orbit doctor`.".to_string(),
                );
            }
        };
    let message = format!(
        "before-PR review: {}. after-landing review: {}",
        switches.before_pr_line(),
        switches.after_landing_line()
    );
    let mut remediation = Vec::new();
    if switches.before_pr.local_route_incompatible {
        remediation.push(
            "`review.before_pr` is on and this workspace ships locally, so every local delivery \
             is refused before it can run. Turn it off with `orbit config set review.before_pr \
             false`, or ship through the PR route. After-landing review is the \
             `delivery-code-review` auto-task, not this switch."
                .to_string(),
        );
    }
    if switches.before_pr.problems.len() > usize::from(switches.before_pr.local_route_incompatible)
    {
        remediation.push(
            "Before-PR review needs `operation.review_crew` set to a crew that resolves on this \
             host; set it, or turn `review.before_pr` off."
                .to_string(),
        );
    }
    if let Some(health) = switches
        .after_landing
        .health
        .as_ref()
        .filter(|health| !health.healthy())
    {
        remediation.push(format!(
            "After-landing review runs only through `{consumer}` on the machine that owns this \
             workspace. Fix each problem named above (`orbit auto-task show {consumer} \
             --preview` shows the consumer), or turn it off with `orbit auto-task toggle \
             {consumer} off`.",
            consumer = health.consumer
        ));
    }
    if remediation.is_empty() {
        return check(CHECK, WorkspaceDoctorStatus::Ok, message);
    }
    remediation.push("Then rerun `orbit doctor`.".to_string());
    actionable_check(
        CHECK,
        WorkspaceDoctorStatus::Error,
        message,
        remediation.join(" "),
    )
}

/// Task relation/dependency targets that no longer resolve to a registered
/// task bundle — the "grandfathered" relations that make a generated task
/// index fail to rebuild against its relation validator, so task reads serve
/// from a bundle scan (ORB-10305). Audited from the canonical bundles and
/// scoped to the current workspace: an edge the generated index lost is the
/// one blocking its repair, so it is reported (and marked) rather than hidden.
pub(super) fn doctor_check_task_relations(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let workspace_id = match runtime.workspace_id() {
        Ok(id) => id,
        Err(error) => {
            return check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!("cannot resolve workspace id to audit relations: {error}"),
            );
        }
    };
    match runtime.audit_dangling_relations(Some(&workspace_id)) {
        Err(error) => check(
            "task-relations",
            WorkspaceDoctorStatus::Warning,
            format!("cannot audit task relations: {error}"),
        ),
        Ok(dangling) if dangling.is_empty() => check(
            "task-relations",
            WorkspaceDoctorStatus::Ok,
            "no unresolved relation/dependency targets".to_string(),
        ),
        Ok(dangling) => {
            let detail = dangling
                .iter()
                .map(|target| {
                    format!(
                        "{} ({}) -> {}{}",
                        target.source_task_id,
                        target.relation_type,
                        target.target_task_id,
                        if target.indexed {
                            ""
                        } else {
                            " [missing from generated index]"
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            actionable_check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!(
                    "{} unresolved relation/dependency target(s) block task-index rebuild, so \
                     task reads fall back to a bundle scan until fixed or removed: {detail}",
                    dangling.len()
                ),
                "Inspect each named source with `orbit task show <task-id>`, then drop the unresolved edge from its `relations` through `orbit.task.update` or restore the target task; reads repair the index automatically once either lands.".to_string(),
            )
        }
    }
}

/// Definition-artifact health — one row per artifact kind (skills, jobs,
/// activities, auto-tasks, routines) [ORB-10800].
///
/// Severity is deliberately asymmetric. A workspace-authored definition that
/// fails to parse is the operator's own in-progress edit, and classifying it
/// as `Error` would silently start failing `orbit doctor` in cron and CI for
/// workspaces that were passing yesterday. Only an *unloadable shipped
/// default* — a broken install: Orbit-written content that no longer parses,
/// or a primary shipped default that is simply gone — escalates to `Error`.
/// Everything else warns.
pub(super) fn doctor_check_definition_artifacts(
    runtime: &OrbitRuntime,
) -> Vec<WorkspaceDoctorResult> {
    let report = match runtime.inspect_definition_artifacts() {
        Ok(report) => report,
        Err(error) => {
            return vec![actionable_check(
                "artifacts",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect definition artifacts: {error}"),
                "Resolve the store/runtime error, then rerun `orbit doctor`.".to_string(),
            )];
        }
    };

    report
        .into_iter()
        .map(|health| {
            let check_name = format!("artifacts-{}", health.kind.as_str());
            if health.findings.is_empty() {
                let message = if health.scanned == 0 {
                    format!("no {} on disk yet", health.kind.as_str())
                } else {
                    format!(
                        "{} {} loaded, none residual, stale, forked, deprecated, faulty, or missing",
                        health.scanned,
                        health.kind.as_str()
                    )
                };
                let status = if health.scanned == 0 {
                    WorkspaceDoctorStatus::Skipped
                } else {
                    WorkspaceDoctorStatus::Ok
                };
                return check(&check_name, status, message);
            }

            let status = if health
                .findings
                .iter()
                .any(ArtifactFinding::is_unloadable_shipped_default)
            {
                WorkspaceDoctorStatus::Error
            } else {
                WorkspaceDoctorStatus::Warning
            };
            let detail = health
                .findings
                .iter()
                .map(|finding| format!("{}: {}", finding.condition.as_str(), finding.detail))
                .collect::<Vec<_>>()
                .join("; ");
            let attention_count = health
                .findings
                .iter()
                .map(|finding| finding.name.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let missing_count = health
                .findings
                .iter()
                .filter(|finding| finding.condition == ArtifactCondition::Missing)
                .map(|finding| finding.name.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            // Missing shipped defaults are absent from `scanned`; include
            // them in the population. Keep the ratio bounded if a catalog
            // probe reports multiple findings for one scanned entry.
            let population = health
                .scanned
                .saturating_add(missing_count)
                .max(attention_count);
            let breakdown = [
                ArtifactCondition::Missing,
                ArtifactCondition::Stale,
                ArtifactCondition::Forked,
                ArtifactCondition::Faulty,
                ArtifactCondition::Residual,
                ArtifactCondition::Deprecated,
                ArtifactCondition::DanglingLink,
            ]
            .into_iter()
            .filter_map(|condition| {
                let count = health
                    .findings
                    .iter()
                    .filter(|finding| finding.condition == condition)
                    .map(|finding| finding.name.as_str())
                    .collect::<BTreeSet<_>>()
                    .len();
                (count > 0).then(|| format!("{count} {}", condition.as_str()))
            })
            .collect::<Vec<_>>()
            .join(", ");
            // Every finding carries its own exact repair command; dedupe so a
            // kind with five stale copies names one command, not five.
            let mut remediations: Vec<&str> = Vec::new();
            for finding in &health.findings {
                let remediation = finding.remediation.as_str();
                if !remediations.contains(&remediation) {
                    remediations.push(remediation);
                }
            }
            actionable_check(
                &check_name,
                status,
                format!(
                    "{attention_count} of {population} {} need attention ({breakdown}) — \
                     {detail}",
                    health.kind.as_str(),
                ),
                remediations.join(" "),
            )
        })
        .collect()
}
