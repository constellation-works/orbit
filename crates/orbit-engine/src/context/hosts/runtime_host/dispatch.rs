macro_rules! dispatch_methods {
    () => {
        /// Dispatch a deterministic action by name. The host looks up `action`
        /// in its registry and returns the action's structured output.
        fn run_deterministic(
            &self,
            action: &str,
            config: &Value,
            input: &Value,
            tool_context: ToolContext,
        ) -> Result<Value, DispatchError>;

        /// Report whether `action` names a deterministic action this host's
        /// registry can actually dispatch.
        ///
        /// [ORB-10385] Catalog assets and the installed binary are separate
        /// artifacts: a workspace can load an activity whose `action:` the running
        /// runtime does not implement. Job validation consults this before the
        /// first step runs, so that skew fails admission instead of being
        /// discovered by a terminal failure hook after a task was admitted and
        /// implemented. A host that cannot enumerate its registry answers
        /// `true` and keeps the pre-ORB-10385 behavior of surfacing the miss at
        /// dispatch as
        /// [`DispatchError::DeterministicActionNotRegistered`]. Reporting `true`
        /// for an unknown action is therefore safe; reporting `false` for a
        /// dispatchable one would reject a healthy job.
        fn has_deterministic_action(&self, action: &str) -> bool;

        /// Resolve the CLI executor command and static args for a given v2
        /// provider name. Workspace / env overrides live
        /// in the host so the engine's CLI runner stays environment-agnostic.
        /// Returning an error is the structured failure path when a provider has no
        /// CLI mapping (e.g. `openai_compat` which is HTTP-only).
        fn resolve_cli_executor(
            &self,
            provider: &str,
        ) -> Result<ResolvedCliExecutor, DispatchError>;

        /// Resolve the registered `local_shell` executor definition backing a
        /// deterministic shell step [ORB-11294].
        ///
        /// Separate from [`RuntimeHost::resolve_cli_executor`] on purpose: that
        /// boundary resolves an *agent* provider and rejects anything that is not
        /// `direct_agent` / `agent_cli`. A shell step carries no model, prompt, or
        /// agent tool authority, so it resolves its own executor family and never
        /// borrows an agent's.
        ///
        /// A host without an executor store answers the empty definition: the activity's own `config` block
        /// then has to name the command outright.
        fn resolve_local_shell_executor(
            &self,
            executor: &str,
        ) -> Result<ResolvedShellExecutor, DispatchError>;

        /// Return provider-specific CLI runtime config for agent execution.
        ///
        /// Most providers ignore this today. Codex uses it for sandbox,
        /// approval-policy, and writable-directory arguments that must stay dynamic
        /// rather than living in the static executor definition.
        fn provider_cli_config(&self, provider: &str) -> HashMap<String, String>;

        /// Resolve the OS sandbox payload for a CLI invocation. The host reads
        /// the executor's `sandbox` declaration, materializes the activity's
        /// `fs_profile` against the active policy, and compiles the result via
        /// `orbit-exec`. Returns `Ok(None)` when the executor has no sandbox
        /// declared (today's behavior). Returns a structured error on
        /// platform mismatch (e.g. `macos-sandbox-exec` on Linux) so the
        /// activity fails closed at dispatch time.
        ///
        /// `subprocess_cwd` is the resolved working directory the subprocess
        /// will run in. The host uses it to re-allow the active worktree path
        /// after the policy's `denyModify .orbit/**` rule when the cwd is a
        /// jrun worktree under `.orbit/state/worktrees/`. Without this, every
        /// non-codex provider (claude/gemini) cannot write inside its own
        /// worktree because the deny rule wins last-match. See T20260508-17.
        fn resolve_executor_sandbox(
            &self,
            provider: &str,
            fs_profile: Option<&str>,
            subprocess_cwd: Option<&Path>,
        ) -> Result<Option<ResolvedSandbox>, DispatchError>;

        /// Run `probe` in the implementer's lane. Hosts must opt into this
        /// capability and apply the sandbox resolved for the provider, profile
        /// and checkout. A host without the capability reports an unsupported,
        /// unexecuted probe, which cannot justify a deferral [ORB-15516].
        fn run_deferral_probe(
            &self,
            probe: &DeferralProbe,
            provider: Option<&str>,
            fs_profile: Option<&str>,
            cwd: &Path,
        ) -> NamespaceProbe;

        /// Optional task snapshot to embed in the CLI agent envelope `step_id` of
        /// `run_id` dispatches.
        ///
        /// The engine keeps this as untyped JSON so orbit-core can source task data
        /// without leaking store or task-query details into orbit-engine. A host
        /// with run storage and `workflow.mid_run_steering.enabled` records, while
        /// building the snapshot, the newest task comment it carries as the run's
        /// steering watermark for that step. At a later prompt of the same run, it
        /// delivers operator comments past that watermark as a leading `steering`
        /// block and records the delivery; otherwise the snapshot is the same
        /// either way.
        fn task_context_for_agent_input(
            &self,
            run_id: &str,
            step_id: &str,
            input: &Value,
        ) -> Result<Option<Value>, DispatchError>;

        /// The `regression_risk` section `activity_name` of `run_id` adds to its
        /// task snapshot: the task's context files that regressed often enough
        /// in the configured trailing window to be marked, highest risk first,
        /// each with its regressed-of-touching counts, nature and escape
        /// categories and one lesson line, and no internal IDs. The engine asks
        /// only for an activity that declares a `regression_risk_instruction`.
        /// `None` when marks are off, nothing qualifies, the input names no
        /// single task, or the host cannot read its records; the host records
        /// each section it returns on the task.
        fn regression_risk_for_agent_input(
            &self,
            run_id: &str,
            activity_name: &str,
            input: &Value,
        ) -> Option<Value>;

        /// Before agent activity `activity_name` of `run_id`, which may edit the
        /// run's worktree, take the context locks the run's admission deferred,
        /// waiting while other work holds them.
        ///
        /// Only a run admitted without its footprint has anything to take: a
        /// resumed published delivery, which adopts its existing branch and pull
        /// request before it edits. Every other run returns at once. The engine
        /// asks for each agent dispatch that may write (not a read-only
        /// `reviewer` profile). A footprint still held when the host's wait ends
        /// refuses the dispatch, so two writers never share a file.
        fn acquire_deferred_footprint(
            &self,
            run_id: &str,
            activity_name: &str,
        ) -> Result<(), DispatchError>;

        /// The tag instructions `activity_name` of `run_id` receives for the task
        /// its `input` names, in application order.
        ///
        /// The text comes only from trusted sources: Orbit's shipped defaults and
        /// repository files read at the delivery's base revision, as the host's
        /// configuration maps the task's tags to them. A claimed leaf's host
        /// answers from what the owner captured at admission. An activity no tag
        /// addresses, or input naming no single task, receives none. A mapped
        /// file that is missing, unreadable or over its size cap refuses the
        /// dispatch with a named error rather than running without it.
        fn tag_instructions_for_activity(
            &self,
            run_id: &str,
            activity_name: &str,
            input: &Value,
        ) -> Result<Vec<TagInstruction>, DispatchError>;

        /// Whether this host offers read-only exploration delegation: the
        /// workspace enables `workflow.explore` and an explorer crew can run on
        /// this host [ORB-16814]. When false, an activity's
        /// `explore_instruction` is never appended.
        fn explore_delegation_available(&self) -> bool;

        /// Whether `workflow.mid_run_steering.enabled` is on for this dispatch.
        /// When false (the default), an activity's `steering_instruction` is
        /// never appended, so rendered prompts stay byte-identical.
        fn mid_run_steering_enabled(&self) -> bool {
            false
        }

        /// Read mid-run steering's promotion gate for `task_id` of `run_id`,
        /// before the step the job's `steering_gate` guards
        /// (`docs/design/mid-run-steering/2_design.md` §3).
        ///
        /// With `workflow.mid_run_steering.enabled` off the host reads nothing
        /// and answers `Disabled`. On, it re-reads the task and its comment log
        /// and judges them against the run's own watermark. A run that holds no
        /// watermark for the task inherits one: from its resume lineage, then
        /// from `candidate_source`, the run whose candidate it carries past its
        /// implementer, which must be a run of the task this host recorded. Past
        /// those it counts every operator comment written after the earliest run
        /// it reached started. The reading comes from those durable records
        /// alone, never from agent output. A record it cannot read, or a
        /// baseline it cannot resolve, is an error, which the gate treats as a
        /// refusal. A claimed leaf's host reads the task, its comment log and
        /// the leaf's watermark from the owner's record, never its replica, and
        /// an owner it cannot read is such an error.
        fn steering_gate(
            &self,
            run_id: &str,
            task_id: &str,
            candidate_source: Option<&str>,
        ) -> Result<SteeringGateReading, OrbitError>;

        /// Admit one more promotion-gate steering round of `run_id` for
        /// `task_id`, whose gate found `outstanding` undelivered from the
        /// baseline `candidate_source` gave it.
        ///
        /// The host records an admitted round, with `candidate_source`, in the
        /// run's state before answering, so the round is spent even when its
        /// dispatch then fails, and its first prompt delivers from the gate's
        /// baseline. It refuses once the run spent
        /// `workflow.mid_run_steering.max_rounds`, or when its last round was
        /// admitted for exactly `outstanding`.
        fn admit_steering_round(
            &self,
            run_id: &str,
            task_id: &str,
            outstanding: &[orbit_types::workflow::CommentLogMark],
            candidate_source: Option<&str>,
        ) -> Result<SteeringRoundAdmission, OrbitError>;
    };
}
pub(super) use dispatch_methods;
