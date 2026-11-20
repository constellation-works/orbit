use super::super::*;
use super::AutoDrainRequest;
use crate::application::job::{JobAdmissionPort, JobDrainPort, ReviewAdmissionHook};
use crate::runtime::run_input::recorded_task_ids;

/// [ORB-10544] Cap on the run history scanned by the in-flight ship guard.
/// Non-terminal runs are always among the newest rows, so a bounded window is
/// enough to spot a duplicate dispatch without walking the whole history.
const SHIP_IN_FLIGHT_SCAN_LIMIT: usize = 200;

/// Legacy profile governing how an omitted ship mode is resolved when the
/// runtime has no workspace binding.
///
/// Surfaces choose only their established profile; the resolution policy
/// itself lives in [`OrbitRuntime::resolve_ship_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShipModeResolutionProfile {
    /// Native CLI legacy: load the registry under the runtime's global root
    /// and accept only a single checkout whose `orbit_dir` equals the
    /// runtime's shared root; that workspace's mode wins. A missing or
    /// ambiguous match warns and falls back to `pr`, and registry load
    /// errors propagate.
    NativeCliLegacyRoot,
    /// Tool and HTTP legacy: no registry lookup; an unbound runtime falls
    /// back to `pr` without a warning.
    BoundRuntimeOnly,
}

/// One `ship` submission: what to ship and the authority this run carries.
///
/// Named fields replace the former positional list, where three adjacent
/// `bool`s granted different authority. Fill the options a surface does not
/// offer with `..ShipRunRequest::default()`.
#[derive(Debug, Clone)]
pub struct ShipRunRequest<'a> {
    pub mode: crate::application::workflow::ShipMode,
    /// Base branch override; `None` uses the workspace's `[workflow] base_branch`.
    pub base_branch: Option<&'a str>,
    /// Explicit task selection; empty selects auto (backlog-discovery) mode.
    pub task_ids: &'a [String],
    pub completion: crate::application::workflow::CompletionPolicy,
    /// Run-scoped crew restriction; empty means unrestricted.
    pub allowed_crews: &'a [String],
    pub actor: Option<&'a str>,
    pub claim_token: Option<&'a str>,
    /// The submitting surface's provenance [ORB-13016].
    pub trigger: JobRunTrigger,
    /// CLI-only strict worker containment override.
    pub strict_containment: bool,
    /// Run-scoped `--review` request [ORB-16572].
    pub review: Option<orbit_types::workflow::ReviewLayer>,
    /// An operator's explicit ship (the CLI and the dashboard's Ship): the
    /// named tasks bypass the drain's readiness gates — a pending
    /// `not_before`, unassessed complexity, no `context_files` — and their
    /// gate's wait on another in-flight task's context locks, and the ship
    /// records which it overrode. Host, crew and live-run refusals still
    /// apply. Ignored without task ids.
    pub operator_override: bool,
}

impl Default for ShipRunRequest<'_> {
    fn default() -> Self {
        Self {
            mode: crate::application::workflow::ShipMode::Pr,
            base_branch: None,
            task_ids: &[],
            completion: crate::application::workflow::CompletionPolicy::Review,
            allowed_crews: &[],
            actor: None,
            claim_token: None,
            trigger: JobRunTrigger::cli(),
            strict_containment: false,
            review: None,
            operator_override: false,
        }
    }
}

impl OrbitRuntime {
    /// Resolve the effective ship mode for a `ship` invocation.
    ///
    /// An explicit mode always wins. Otherwise a bound runtime uses its
    /// workspace binding's mode. Only an unbound runtime consults
    /// `profile`: see [`ShipModeResolutionProfile`] for the two legacy
    /// policies this preserves.
    pub fn resolve_ship_mode(
        &self,
        explicit: Option<crate::application::workflow::ShipMode>,
        profile: ShipModeResolutionProfile,
    ) -> Result<crate::application::workflow::ShipMode, OrbitError> {
        if let Some(mode) = explicit {
            return Ok(mode);
        }
        if let Some(binding) = self.workspace_runtime_binding() {
            return Ok(binding.ship_mode);
        }
        match profile {
            ShipModeResolutionProfile::BoundRuntimeOnly => {
                Ok(crate::application::workflow::ShipMode::Pr)
            }
            ShipModeResolutionProfile::NativeCliLegacyRoot => {
                let registry_path =
                    orbit_registry::workspace_registry::registry_path_for(&self.global_root());
                let registry =
                    orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
                let orbit_dir = self.shared_root();
                let mut checkouts = registry
                    .checkouts
                    .iter()
                    .filter(|checkout| checkout.orbit_dir == orbit_dir);
                let checkout = checkouts.next().filter(|_| checkouts.next().is_none());
                let mode = checkout
                    .and_then(|checkout| {
                        registry
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.id == checkout.workspace_id)
                    })
                    .map(crate::application::workflow::resolved_ship_mode);
                let Some(mode) = mode else {
                    tracing::warn!(
                        registry_path = %registry_path.display(),
                        orbit_dir = %orbit_dir.display(),
                        fallback = "pr",
                        "workspace was not found in registry; falling back to PR ship mode"
                    );
                    return Ok(crate::application::workflow::ShipMode::Pr);
                };
                Ok(mode)
            }
        }
    }

    /// Submit a `ship` workflow run (the `task_auto_pipeline` job).
    ///
    /// Shared entry point for every non-interactive submission surface
    /// (dashboard HTTP endpoint, MCP `orbit.workflow.ship`). `base_branch`
    /// falls back to the workspace's `[workflow] base_branch`; an empty
    /// `task_ids` slice selects auto (backlog-discovery) mode. One-shot: returns as soon as the run is
    /// persisted and its worker spawned.
    ///
    /// [ORB-10544] An explicit task selection is guarded against duplicate
    /// dispatch here rather than in any one adapter: if a named task is already
    /// carried by a non-terminal delivery run, the submission is refused with
    /// [`OrbitError::ShipRunInFlight`] naming that task and run, so two runs
    /// cannot contend for one worktree and task reservation no matter which
    /// surface submitted them. Auto mode has no task ids to key on and is
    /// unaffected.
    ///
    /// An explicit task that selects its delivery job with a `delivery:<job>`
    /// tag is refused here, before any run exists, when that job cannot
    /// deliver it in `mode` — including when the plugin contributing it is
    /// disabled or uninstalled. The gate resolves the same selection again
    /// before it reserves anything. A task the PR pipeline would deliver is
    /// refused with [`OrbitError::PrForgeRemoteMissing`] when no Git remote of
    /// the checkout names a network host, since `pr_open` could never succeed.
    ///
    /// [ORB-11187] `completion` is the caller's explicit authorization for this
    /// run to finish delivery and perform the guarded `review -> done`
    /// transition. It defaults to
    /// [`CompletionPolicy::Review`](crate::application::workflow::CompletionPolicy::Review)
    /// at every surface and is only ever raised by a per-invocation operator
    /// flag; nothing derives it from workspace configuration or the environment.
    ///
    /// [ORB-10709] The workspace claim is checked first, for the case the
    /// duplicate-dispatch guard structurally cannot cover: it is keyed on task
    /// id over a bounded window of recent runs, so a stale non-terminal run
    /// outside that window is invisible to it, and a discovery-mode submission
    /// carries no task ids at all. The claim check is keyed on neither, so both
    /// gaps close. `claim_token` is the holder's minted token; `None` falls back
    /// to [`CLAIM_TOKEN_ENV`](crate::runtime::workspace::claim::CLAIM_TOKEN_ENV).
    ///
    /// [ORB-13016] `trigger` is the submitting surface's provenance, recorded
    /// on the run so dashboard and MCP launches are not reported as `cli`.
    ///
    /// `strict_containment` is a CLI-only override: the marker is persisted
    /// with this run's input and its worker passes the policy through the
    /// environment to any child workers it starts. [ORB-16572] `review` is
    /// the run's `--review` request, refused here, before any run exists,
    /// on the local route, against the other workspace layer, or without a
    /// reviewer crew. A named `backlog` task whose `not_before` is still
    /// ahead is refused with [`OrbitError::TaskScheduled`] unless
    /// `operator_override` is set: an operator's explicit ship (CLI and
    /// dashboard; the tool surface passes `false`) overrides the drain's
    /// readiness gates. Once every other check passes, it records a
    /// `schedule_overridden` event for each task scheduled ahead and a
    /// `readiness_overridden` event naming the gates each task failed,
    /// before the run is submitted, and marks the run input so its listing
    /// step skips those gates and its gate does not wait on context locks.
    pub fn submit_ship_run(
        &self,
        request: ShipRunRequest<'_>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let ShipRunRequest {
            mode,
            base_branch,
            task_ids,
            completion,
            allowed_crews,
            actor,
            claim_token,
            trigger,
            strict_containment,
            review,
            operator_override,
        } = request;
        // The override names its own work; a discovery ship is drain policy.
        let operator_override = operator_override && !task_ids.is_empty();
        self.validate_strict_worker_containment(strict_containment)?;
        self.require_workspace_claim("orbit.workflow.ship", claim_token)?;
        // [ORB-25745] A duplicate of a task a live run already carries is refused
        // as that run, before the claim ledger's entry refusal below can name the
        // task's execution claim instead: a run past admission holds that claim too.
        if let Some(conflict) = self.in_flight_ship_run_for_tasks(task_ids)? {
            return Err(conflict);
        }
        // [ORB-12500] Explicit shipment converges on the same admission
        // decision the drain, the retained sweep and pull admission make: a
        // replica serves no owner coordination, and a task a live claim is
        // already executing is settled or recovered, never shipped beside
        // itself. Saturation does not stand an operator's explicit invocation
        // down — the leaf definition's own `max_active_runs` bounds it.
        self.admit_explicit_drain_entry(
            crate::application::contracts::DrainEntryPoint::ExplicitShip,
            task_ids,
        )?;
        let workflow_job_id = orbit_types::workflow::TASK_AUTO_PIPELINE_JOB;
        let base = base_branch.unwrap_or_else(|| self.workspace_base_branch());
        let allowed_crews = self.canonical_allowed_crews(allowed_crews)?;
        let allowlist = self.crew_allowlist(&allowed_crews)?;
        let mut input = crate::application::workflow::build_ship_input(
            mode,
            base,
            task_ids,
            completion,
            &allowed_crews,
        )?;
        if strict_containment {
            input["__worker_containment_strict"] = json!(true);
        }
        if operator_override {
            input[crate::application::job::OPERATOR_READINESS_OVERRIDE_INPUT] = json!(true);
        }
        self.admit_review_request(
            &mut input,
            review,
            mode == crate::application::workflow::ShipMode::Local,
        )?;
        // Validate explicit selections before inspecting runs or creating a
        // pipeline record. Auto mode intentionally carries no task ids: the
        // worker discovers eligible backlog tasks after it starts.
        let forge = crate::application::job::delivery::PrForgeCheck::default();
        let mut overridden = Vec::new();
        for task_id in task_ids {
            // A task the pipeline cannot start from (`blocked`, `review`,
            // `done`, `proposed`, ...) would only be discovered after the
            // run was accepted, reported `submitted`, and then failed or
            // no-oped at the gate. Refuse it here, naming the status and the
            // way back into the backlog. A task scheduled ahead is refused
            // the same way unless an operator's ship overrides its schedule.
            let task = self.ensure_task_can_enter_workflow_honouring_schedule(
                task_id,
                crate::application::workflow::SHIP_WORKFLOW_ALIAS,
                !operator_override,
            )?;
            if operator_override {
                let hold = self.schedule_hold(&task)?;
                let gates =
                    JobAdmissionPort::operator_overridden_gates(self, &task, hold.is_some());
                if !gates.is_empty() {
                    overridden.push((task.id.clone(), hold.map(|hold| hold.not_before), gates));
                }
            }
            // A task whose effective OS requirement this host cannot satisfy is
            // never started here; it would only fail on a platform it cannot
            // run on. Check its tags and assigned crew, and the actual
            // default fallback when its automatic route has no usable pool.
            let limits = self.executor_host_os_limits()?;
            let pools = self.auto_crew_pools_for_input(&input)?;
            let wait = self.task_host_os_wait(&task, &pools, &limits);
            if let Some(wait) = wait {
                return Err(OrbitError::PolicyDenied(format!(
                    "explicit ship task '{task_id}' {wait}, and this host runs {}; ship it on \
                     a host of that OS (a pull-drain follower on one claims it from the \
                     backlog) or change its `os:` tags",
                    self.host_os()
                        .map_or(std::env::consts::OS, orbit_types::task::HostOs::as_str)
                )));
            }
            // Nor a task requiring a capability label this host lacks.
            if let Some(wait) = self.task_host_labels_wait(&task) {
                return Err(OrbitError::PolicyDenied(format!(
                    "explicit ship task '{task_id}' {wait}; ship it on a host whose \
                     `machine.labels` carry them (a pull-drain follower that does claims it \
                     from the backlog) or change its `requires_labels`"
                )));
            }
            // Nor a `ui` task on a host whose browser kit is not verified: its
            // worker could not render the change it must screenshot.
            if orbit_types::task::requires_browser_kit(&task.tags) && !self.browser_kit_verified() {
                return Err(OrbitError::PolicyDenied(format!(
                    "explicit ship task '{task_id}' {}; this host has no verified kit (a pull-drain \
                     follower with one claims it from the backlog)",
                    orbit_types::task::browser_kit_wait()
                )));
            }
            if let Some(allowlist) = allowlist.as_ref() {
                // [ORB-12606] Report against the crew admission will draw, not
                // the default chain: a crew-less task whose complexity pool
                // still has a permitted member is not excluded.
                self.enforce_admitted_crew_allowlist(
                    &task,
                    &input,
                    allowlist,
                    &format!("explicit ship task '{task_id}'"),
                )?;
            }
            let route =
                self.resolve_admitted_delivery_route(std::slice::from_ref(&task), mode, &forge)?;
            if let Some(layer) = review
                && route.delivers_locally()
            {
                return Err(OrbitError::InvalidInput(format!(
                    "explicit ship task '{task_id}' is routed to local-only delivery: {}",
                    self.local_route_review_refusal(layer)
                )));
            }
        }
        // Preview the same exclusions as dispatch without reserving an owner
        // wave. A mixed request still reaches the worker with all IDs so its
        // audit records every omission.
        if !task_ids.is_empty() {
            let preview = JobAdmissionPort::preview_named_ship(self, &input)?;
            if preview["task_ids"].as_array().is_some_and(Vec::is_empty) {
                return Err(OrbitError::PolicyDenied(format!(
                    "named ship has no dispatchable tasks: {}; resolve the listed holds and retry",
                    preview["excluded"]
                )));
            }
        }
        // The worker starts as soon as the run is submitted, so the override
        // is on record before then.
        let overrider = actor.unwrap_or(&self.actor().label).to_string();
        for (task_id, not_before, gates) in &overridden {
            if let Some(not_before) = not_before {
                self.record_schedule_override(task_id, *not_before, &overrider)?;
            }
            JobAdmissionPort::record_readiness_override(self, task_id, gates, &overrider)?;
        }
        self.submit_pipeline_run_with_trigger(workflow_job_id, input, None, actor, trigger)
    }

    /// Submit a local drain with a run-scoped forecast mode. Omission captures
    /// the host setting; explicit modes never edit configuration. All surfaces
    /// use this submission and the existing admission controller.
    ///
    /// [ORB-10819] `for_seconds` is the drain window: the run keeps re-listing
    /// admissible work and shipping it until the window expires. `None` (or
    /// zero) means one tick, which is what every caller predating the window
    /// gets. Windows over 24 hours are refused before submission. The window
    /// bounds only the *start* of new work — a child run
    /// already in flight when the deadline passes finishes normally.
    /// `max_active_leaf_runs` is the drain's concurrency ceiling: how many
    /// `task_auto_pipeline` children may be live at once. Omitted, the job's
    /// own default applies — this only forwards an explicit override, so the
    /// default lives in one place, next to the loop that reads it.
    ///
    /// [ORB-11242] `allowed_crews` is the run-scoped crew restriction. Empty
    /// means unrestricted, which is what every caller predating it gets. Names
    /// are resolved against this host's `[crews.*]` registry *here*, before a
    /// run record exists, so an unknown or blank name fails the submission
    /// rather than quietly shrinking what a live drain admits; the canonical
    /// registry names are what gets persisted and forwarded. It gates what the
    /// drain may *start* — it does not touch workspace configuration, reassign
    /// a task's crew, or cancel work another invocation already has in flight.
    ///
    /// [ORB-13016] `trigger` is the submitting surface's provenance.
    ///
    /// [ORB-14117] `approve_proposed` lets every pass of this drain approve
    /// qualifying `proposed` tasks into the backlog through the task-pilot
    /// promotion boundary. Like `completion`, it is only ever raised by a
    /// per-invocation operator flag.
    ///
    /// `auto_fix` (`--auto-fix`) authorizes the drain's PR leaves to repair
    /// failed required checks and merge conflicts on their pull requests. It
    /// is repair authority only; `completion` alone authorizes merging.
    ///
    /// [ORB-16572] `review` (`--review`) adds a review layer before landing to
    /// the window where workspace policy applies none; every child it admits
    /// inherits the captured layer. It is refused before submission when the
    /// workspace delivers locally, and as on ship otherwise.
    pub fn submit_workspace_auto_run(
        &self,
        request: AutoDrainRequest<'_>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        crate::application::workflow::validate_drain_window(request.for_seconds)?;
        self.validate_strict_worker_containment(request.strict_containment)?;
        self.require_workspace_claim("orbit.workflow.auto", request.claim_token)?;
        // [ORB-12500] An explicit owner drain is owner coordination work; a
        // replica executes through pull instead of running one locally.
        self.admit_explicit_drain_entry(
            crate::application::contracts::DrainEntryPoint::OwnerDrain,
            &[],
        )?;
        self.drain_git_protection_preflight()?;
        let crews =
            self.drain_crews_runnable_here(self.canonical_allowed_crews(request.allowed_crews)?)?;
        let workflow_job_id = orbit_types::workflow::WORKSPACE_AUTO_PIPELINE_JOB;
        let mut input = workspace_auto_run_input(&request, &crews)?;
        if let Some(mode) = request.auto_concurrency {
            input["auto_concurrency"] = json!(mode);
        }
        Self::set_auto_crew_overrides(&mut input, &request.complexity_crews);
        if request.strict_containment {
            input["__worker_containment_strict"] = json!(true);
        }
        self.admit_review_request(
            &mut input,
            request.review,
            self.automatic_delivery_ship_mode() == crate::application::workflow::ShipMode::Local,
        )?;
        self.submit_pipeline_run_with_trigger(
            workflow_job_id,
            input,
            None,
            request.actor,
            request.trigger,
        )
    }

    fn validate_strict_worker_containment(&self, strict: bool) -> Result<(), OrbitError> {
        if strict && !self.context.settings().worker_containment().enabled {
            return Err(OrbitError::InvalidInput(
                "--strict-worker-containment requires machine.worker_containment to be warn or strict, not off".into(),
            ));
        }
        Ok(())
    }

    /// Canonicalize an operator-supplied crew allowlist, rejecting blank or
    /// unconfigured names [ORB-11242].
    ///
    /// Canonical registry names are persisted rather than the operator's
    /// spelling, so the durable run input says exactly which configured crews
    /// the window permits regardless of the alias that was typed.
    pub(crate) fn canonical_allowed_crews(
        &self,
        allowed_crews: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let mut canonical: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for name in allowed_crews {
            if name.trim().is_empty() {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            }
            // A human crew is accepted and admits nothing; readiness names it.
            let Some(resolved) = self.canonical_task_crew_name(Some(name))? else {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            };
            canonical.insert(resolved);
        }
        Ok(canonical.into_iter().collect())
    }

    /// The duplicate-dispatch refusal for the newest non-terminal delivery run
    /// carrying one of `task_ids`, or `None` when the selection is free.
    ///
    /// A run's task selection lives in its persisted `input.task_ids`, which is
    /// what [`Self::submit_ship_run`] writes, so this sees every prior
    /// submission regardless of the surface that made it.
    ///
    /// Only runs of jobs declaring `spec.task_delivery` hold the slot: the ship
    /// coordinators, the delivery leaves, and any plugin delivery job. The
    /// workspace drain and preparation jobs may list task IDs without holding
    /// it; their dispatched children do.
    pub(crate) fn in_flight_ship_run_for_tasks(
        &self,
        task_ids: &[String],
    ) -> Result<Option<OrbitError>, OrbitError> {
        if task_ids.is_empty() {
            return Ok(None);
        }
        let delivery_jobs = self.task_delivery_job_ids()?;
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            limit: Some(SHIP_IN_FLIGHT_SCAN_LIMIT),
            ..Default::default()
        })?;
        Ok(runs.into_iter().find_map(|run| {
            if run.state.is_terminal() || !delivery_jobs.contains(&run.job_id) {
                return None;
            }
            let task_id = run
                .input
                .as_ref()
                .map(recorded_task_ids)
                .into_iter()
                .flatten()
                .find(|candidate| task_ids.iter().any(|wanted| wanted == candidate))
                .map(str::to_string)?;
            Some(OrbitError::ShipRunInFlight {
                task_id,
                run_id: run.run_id,
            })
        }))
    }
}
