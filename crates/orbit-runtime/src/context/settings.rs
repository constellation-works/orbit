use super::actor::ActorIdentity;
use orbit_config::PersistenceConfig;
use orbit_engine::PrConfig;
use orbit_types::identity::Crew;

#[derive(Clone)]
pub struct OrbitRuntimeSettings {
    pub(super) persistence: PersistenceConfig,
    pub(super) actor: ActorIdentity,
    /// Minutes a deferred delivery-automation reason may persist before the
    /// evaluator escalates it (`[automation] stall_window_minutes`).
    automation_stall_window_minutes: u32,
    publication_routine_attachments: String,
    publication_routine_allow_unscanned_attachments: bool,
    pr_config: PrConfig,
    /// Config-owned `[pr]` settings; the PR closure on a task's terminal
    /// decision reads `close_on_terminal` and `delivery_authors` from here.
    pr_settings: orbit_config::PrSettings,
    /// Config-only `[workflow] base_branch` fallback (default `"main"`).
    workflow_base_branch: String,
    resource_throttle: orbit_config::ResourceThrottleSettings,
    /// Initial global `[machine.auto_concurrency]` policy, used as the
    /// admission cache's last-known-good fallback before its first refresh.
    auto_concurrency: orbit_config::AutoConcurrencySettings,
    /// Commands this owner requires a distributed execution claim to pass on
    /// its exact candidate before the delivery handoff is accepted
    /// (`[workflow] required_validation_commands`, default empty).
    workflow_required_validation_commands: Vec<String>,
    /// Commands review settlement may rerun to confirm a red-base claim
    /// (`[review] baseline_commands`, default empty) [ORB-14434].
    review_baseline_commands: Vec<String>,
    /// `[workflow.validation_env]`: how required validation and `local_shell`
    /// resolve PATH and toolchain locators [ORB-13987].
    validation_env: orbit_exec::ValidationEnvPolicy,
    /// How far this owner takes an accepted distributed handoff: `review` or
    /// `done` (`[workflow] distributed_completion`, default `review`).
    workflow_distributed_completion: String,
    /// How far admission lets work only the requesting executor can run
    /// lead (`[workflow] admission_affinity`, default `band`).
    admission_affinity: orbit_types::task::AdmissionAffinityReach,
    /// `[workflow.task_pilot_freshness]`: the global layer a task-pilot
    /// routine's `trigger.state.freshness` overrides [ORB-13638].
    task_pilot_freshness: orbit_types::workflow::automation::members::PreparationFreshness,
    crews: std::collections::BTreeMap<String, Crew>,
    /// Operator crews (`[crews.<name>] kind = "human"`), apart from `crews`.
    human_crews: std::collections::BTreeMap<String, orbit_types::identity::HumanCrew>,
    default_crew: Option<String>,
    complexity_crews: orbit_config::ComplexityCrewPools,
    /// Admitted `workflow.purpose_crews.final_recovery`; empty disables final recovery.
    final_recovery_crews: Vec<String>,
    /// Admitted `workflow.purpose_crews.<purpose>` pools; empty is unset.
    purpose_crews: orbit_config::PurposeCrewPools,
    /// Admitted `workflow.type_complexity_crews.<type>.<tier>` pools.
    type_complexity_crews: orbit_config::TypeComplexityCrewPools,
    /// Admitted `workflow.auto_effort.*`: the bands `<crew>@auto` pool
    /// members resolve through, and the exploration share.
    auto_effort: orbit_config::AutoEffortPolicy,
    /// Initial `workflow.provider_limit_*` policy for the admission cache's
    /// last-known-good fallback before its first refresh.
    provider_limit: orbit_config::ProviderLimitPolicy,
    /// Resolved `[operation]` review preferences with provenance [ORB-11333].
    operation: orbit_config::OperationPolicy,
    /// Global `machine.worker_*` limits for detached workers [ORB-12903].
    worker_containment: orbit_config::WorkerContainmentSettings,
    /// Existing machine identity's task namespace; ORB for legacy runtimes.
    machine_task_prefix: String,
    /// `machine.host_shutdown_hold_minutes`: how close a scheduled shutdown
    /// must be before unattended admission holds.
    host_shutdown_hold_minutes: u32,
    worktree_reclaim: Vec<String>,
    worktree_reclaim_below_free_mib: Option<u64>,
    worktree_parked_reclaim_after_hours: u64,
    /// `[workflow.mid_run_steering]`: whether task-context prompts record
    /// the run's steering watermark (default off).
    mid_run_steering: orbit_config::MidRunSteeringSettings,
    /// `[workflow.tag_instructions]`: the files that replace or disable
    /// Orbit's shipped per-tag instructions.
    tag_instructions: orbit_config::TagInstructionSettings,
    /// `[workflow.review_skip_tags]`: mapped tags whose candidates may
    /// skip agent review under their own paths and guard.
    review_skip_tags: orbit_config::ReviewSkipSettings,
    attention: orbit_config::AttentionSettings,
    /// `workflow.unlocked_context_paths`: declared context that takes no
    /// context lock.
    unlocked_context_paths: crate::task::locks::UnlockedContextPaths,
    /// `workflow.lock_modes`: the paths where a footprint overlap is shared
    /// rather than blocking.
    footprint_lock_modes: orbit_common::fs::lock_mode::FootprintLockModes,
    /// `workflow.unlocked_path_validation_commands`: what a candidate that
    /// changed only unlocked paths since its run's last full validation runs.
    unlocked_path_validation_commands: Vec<String>,
    /// `workflow.generated_paths`: path glob to regenerate command.
    generated_paths: std::collections::BTreeMap<String, String>,
    /// `workflow.light_delivery_paths`: the paths a `skip-review` or
    /// `skip-validation` candidate may change [ORB-16855].
    light_delivery_paths: Vec<String>,
    /// `workflow.test_paths` and `workflow.non_code_paths` as compiled
    /// filters: how a landed diff's paths split into tests, other and code.
    /// `workflow.non_reviewable_paths` drops paths from the reviewable-lines
    /// denominator without changing that class split.
    diff_test_paths: orbit_common::fs::path_filter::PathFilter,
    diff_non_code_paths: orbit_common::fs::path_filter::PathFilter,
    diff_non_reviewable_paths: orbit_common::fs::path_filter::PathFilter,
    /// `workflow.task_types`: the ordered task types type pools route by
    /// and the implementer scorecard's by-type lens.
    task_types: Vec<orbit_config::TaskTypeDefinition>,
    /// `[decision_channel] enabled`: raise decision requests for held work.
    decision_channel_enabled: bool,
    /// The fixed lease policy (path leases disabled) [ORB-22808]: whether
    /// path leases are accepted and the per-class guards a request is held to.
    lease_policy: crate::task::locks::LeasePolicy,
    /// `experiments.*`: the workspace's experiment policy.
    experiments: orbit_config::ExperimentSettings,
    /// `notice.*`: fleet notice expiry, limits and brief delivery.
    notice: orbit_config::NoticeSettings,
    /// Admitted `machine.labels`: the capability labels this host carries,
    /// beside its OS as an implicit label.
    machine_labels: Vec<String>,
    /// Admitted `machine.application_consent_max_days`: the longest lifetime
    /// of a constrained (schema v2) application consent.
    application_consent_max_days: u32,
}

impl OrbitRuntimeSettings {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        persistence: PersistenceConfig,
        actor: ActorIdentity,
        automation_stall_window_minutes: u32,
        publication_routine_attachments: String,
        publication_routine_allow_unscanned_attachments: bool,
        pr_config: PrConfig,
        pr_settings: orbit_config::PrSettings,
        workflow_base_branch: String,
        resource_throttle: orbit_config::ResourceThrottleSettings,
        auto_concurrency: orbit_config::AutoConcurrencySettings,
        workflow_required_validation_commands: Vec<String>,
        review_baseline_commands: Vec<String>,
        validation_env: orbit_exec::ValidationEnvPolicy,
        workflow_distributed_completion: String,
        admission_affinity: orbit_types::task::AdmissionAffinityReach,
        task_pilot_freshness: orbit_types::workflow::automation::members::PreparationFreshness,
        crews: std::collections::BTreeMap<String, Crew>,
        human_crews: std::collections::BTreeMap<String, orbit_types::identity::HumanCrew>,
        default_crew: Option<String>,
        complexity_crews: orbit_config::ComplexityCrewPools,
        final_recovery_crews: Vec<String>,
        purpose_crews: orbit_config::PurposeCrewPools,
        type_complexity_crews: orbit_config::TypeComplexityCrewPools,
        auto_effort: orbit_config::AutoEffortPolicy,
        provider_limit: orbit_config::ProviderLimitPolicy,
        operation: orbit_config::OperationPolicy,
        worker_containment: orbit_config::WorkerContainmentSettings,
        machine_task_prefix: String,
        host_shutdown_hold_minutes: u32,
        worktree_reclaim: Vec<String>,
        worktree_reclaim_below_free_mib: Option<u64>,
        worktree_parked_reclaim_after_hours: u64,
        mid_run_steering: orbit_config::MidRunSteeringSettings,
        tag_instructions: orbit_config::TagInstructionSettings,
        review_skip_tags: orbit_config::ReviewSkipSettings,
        attention: orbit_config::AttentionSettings,
        unlocked_context_paths: crate::task::locks::UnlockedContextPaths,
        footprint_lock_modes: orbit_common::fs::lock_mode::FootprintLockModes,
        unlocked_path_validation_commands: Vec<String>,
        generated_paths: std::collections::BTreeMap<String, String>,
        light_delivery_paths: Vec<String>,
        diff_path_filters: (
            orbit_common::fs::path_filter::PathFilter,
            orbit_common::fs::path_filter::PathFilter,
            orbit_common::fs::path_filter::PathFilter,
        ),
        task_types: Vec<orbit_config::TaskTypeDefinition>,
        decision_channel_enabled: bool,
        lease_policy: crate::task::locks::LeasePolicy,
        experiments: orbit_config::ExperimentSettings,
        notice: orbit_config::NoticeSettings,
        machine_labels: Vec<String>,
        application_consent_max_days: u32,
    ) -> Self {
        Self {
            persistence,
            actor,
            automation_stall_window_minutes,
            publication_routine_attachments,
            publication_routine_allow_unscanned_attachments,
            pr_config,
            pr_settings,
            workflow_base_branch,
            resource_throttle,
            auto_concurrency,
            workflow_required_validation_commands,
            review_baseline_commands,
            validation_env,
            workflow_distributed_completion,
            admission_affinity,
            task_pilot_freshness,
            crews,
            human_crews,
            default_crew,
            complexity_crews,
            final_recovery_crews,
            purpose_crews,
            type_complexity_crews,
            auto_effort,
            provider_limit,
            operation,
            worker_containment,
            machine_task_prefix,
            host_shutdown_hold_minutes,
            worktree_reclaim,
            worktree_reclaim_below_free_mib,
            worktree_parked_reclaim_after_hours,
            mid_run_steering,
            tag_instructions,
            review_skip_tags,
            attention,
            unlocked_context_paths,
            footprint_lock_modes,
            unlocked_path_validation_commands,
            generated_paths,
            light_delivery_paths,
            diff_test_paths: diff_path_filters.0,
            diff_non_code_paths: diff_path_filters.1,
            diff_non_reviewable_paths: diff_path_filters.2,
            task_types,
            decision_channel_enabled,
            lease_policy,
            experiments,
            notice,
            machine_labels,
            application_consent_max_days,
        }
    }

    pub(crate) fn machine_labels(&self) -> &[String] {
        &self.machine_labels
    }

    pub(crate) fn application_consent_max_days(&self) -> u32 {
        self.application_consent_max_days
    }

    pub(crate) fn set_machine_labels(&mut self, labels: Vec<String>) {
        self.machine_labels = labels;
    }

    pub(crate) fn decision_channel_enabled(&self) -> bool {
        self.decision_channel_enabled
    }

    pub(crate) fn lease_policy(&self) -> &crate::task::locks::LeasePolicy {
        &self.lease_policy
    }

    pub(crate) fn experiments(&self) -> &orbit_config::ExperimentSettings {
        &self.experiments
    }

    pub(crate) fn notice(&self) -> &orbit_config::NoticeSettings {
        &self.notice
    }

    pub(crate) fn task_types(&self) -> &[orbit_config::TaskTypeDefinition] {
        &self.task_types
    }

    /// Admitted attachment policy for scheduled task publication.
    pub fn publication_routine_attachments(&self) -> &str {
        &self.publication_routine_attachments
    }

    /// Whether the operator explicitly accepts unscanned routine attachments.
    pub fn publication_routine_allow_unscanned_attachments(&self) -> bool {
        self.publication_routine_allow_unscanned_attachments
    }

    pub(crate) fn mid_run_steering(&self) -> orbit_config::MidRunSteeringSettings {
        self.mid_run_steering
    }

    pub(crate) fn tag_instructions(&self) -> &orbit_config::TagInstructionSettings {
        &self.tag_instructions
    }

    pub(crate) fn review_skip_tags(&self) -> &orbit_config::ReviewSkipSettings {
        &self.review_skip_tags
    }

    /// Shared admitted attention configuration.
    pub fn attention_settings(&self) -> &orbit_config::AttentionSettings {
        &self.attention
    }

    pub fn unlocked_context_paths(&self) -> &crate::task::locks::UnlockedContextPaths {
        &self.unlocked_context_paths
    }

    pub fn footprint_lock_modes(&self) -> &orbit_common::fs::lock_mode::FootprintLockModes {
        &self.footprint_lock_modes
    }

    pub fn unlocked_path_validation_commands(&self) -> &[String] {
        &self.unlocked_path_validation_commands
    }

    pub fn generated_paths(&self) -> &std::collections::BTreeMap<String, String> {
        &self.generated_paths
    }

    pub(crate) fn light_delivery_paths(&self) -> &[String] {
        &self.light_delivery_paths
    }

    pub(crate) fn diff_test_paths(&self) -> &orbit_common::fs::path_filter::PathFilter {
        &self.diff_test_paths
    }

    pub(crate) fn diff_non_code_paths(&self) -> &orbit_common::fs::path_filter::PathFilter {
        &self.diff_non_code_paths
    }

    pub(crate) fn diff_non_reviewable_paths(&self) -> &orbit_common::fs::path_filter::PathFilter {
        &self.diff_non_reviewable_paths
    }

    pub(crate) fn set_host_shutdown_hold_minutes(&mut self, minutes: u32) {
        self.host_shutdown_hold_minutes = minutes;
    }

    pub(crate) fn host_shutdown_hold_minutes(&self) -> u32 {
        self.host_shutdown_hold_minutes
    }

    pub fn worktree_reclaim(&self) -> &[String] {
        &self.worktree_reclaim
    }

    pub fn worktree_reclaim_below_free_mib(&self) -> Option<u64> {
        self.worktree_reclaim_below_free_mib
    }

    pub fn worktree_parked_reclaim_after_hours(&self) -> u64 {
        self.worktree_parked_reclaim_after_hours
    }

    pub(crate) fn operation(&self) -> &orbit_config::OperationPolicy {
        &self.operation
    }

    pub fn worker_containment(&self) -> &orbit_config::WorkerContainmentSettings {
        &self.worker_containment
    }

    pub fn machine_task_prefix(&self) -> &str {
        &self.machine_task_prefix
    }

    pub(crate) fn automation_stall_window_minutes(&self) -> u32 {
        self.automation_stall_window_minutes
    }

    pub(crate) fn pr_config(&self) -> &PrConfig {
        &self.pr_config
    }

    pub fn pr_settings(&self) -> &orbit_config::PrSettings {
        &self.pr_settings
    }

    pub(crate) fn workflow_base_branch(&self) -> &str {
        &self.workflow_base_branch
    }

    pub fn resource_throttle(&self) -> &orbit_config::ResourceThrottleSettings {
        &self.resource_throttle
    }

    pub(crate) fn auto_concurrency(&self) -> &orbit_config::AutoConcurrencySettings {
        &self.auto_concurrency
    }

    pub(crate) fn workflow_required_validation_commands(&self) -> &[String] {
        &self.workflow_required_validation_commands
    }

    pub(crate) fn review_baseline_commands(&self) -> &[String] {
        &self.review_baseline_commands
    }

    pub fn validation_env(&self) -> &orbit_exec::ValidationEnvPolicy {
        &self.validation_env
    }

    pub(crate) fn workflow_distributed_completion(&self) -> &str {
        &self.workflow_distributed_completion
    }

    pub(crate) fn admission_affinity(&self) -> orbit_types::task::AdmissionAffinityReach {
        self.admission_affinity
    }

    pub(crate) fn task_pilot_freshness(
        &self,
    ) -> &orbit_types::workflow::automation::members::PreparationFreshness {
        &self.task_pilot_freshness
    }

    pub fn crews(&self) -> &std::collections::BTreeMap<String, Crew> {
        &self.crews
    }

    /// Operator crews, which no dispatch, pool or redraw ever selects.
    pub fn human_crews(
        &self,
    ) -> &std::collections::BTreeMap<String, orbit_types::identity::HumanCrew> {
        &self.human_crews
    }

    pub fn complexity_crews(&self) -> &orbit_config::ComplexityCrewPools {
        &self.complexity_crews
    }

    pub fn final_recovery_crews(&self) -> &[String] {
        &self.final_recovery_crews
    }

    pub fn purpose_crews(&self) -> &orbit_config::PurposeCrewPools {
        &self.purpose_crews
    }

    pub fn type_complexity_crews(&self) -> &orbit_config::TypeComplexityCrewPools {
        &self.type_complexity_crews
    }

    /// The admitted `workflow.auto_effort.*` policy.
    pub fn auto_effort(&self) -> &orbit_config::AutoEffortPolicy {
        &self.auto_effort
    }

    pub(crate) fn provider_limit(&self) -> &orbit_config::ProviderLimitPolicy {
        &self.provider_limit
    }

    pub fn default_crew(&self) -> Option<&str> {
        self.default_crew.as_deref()
    }
}
