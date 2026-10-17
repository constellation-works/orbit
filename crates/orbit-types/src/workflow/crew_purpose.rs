//! The purposes a crew is chosen for by a pool of its own.
//!
//! Each purpose names one weighted pool, written like the complexity pools.
//! An explicit crew always wins over it, and a purpose whose pool is unset
//! resolves exactly as it did before purposes existed: [`CrewPurpose::fallback`]
//! names that route.
//!
//! Final recovery and automatic review already had a pool of their own, so
//! their purpose pool is that existing key (`workflow.final_recovery_crews`,
//! `operation.review_crew`). Every other purpose reads
//! `workflow.purpose_crews.<purpose>`.

/// Prefix of every `workflow.purpose_crews.<purpose>` configuration key.
pub const PURPOSE_CREWS_KEY_PREFIX: &str = "workflow.purpose_crews.";

/// Tag prefix that names a task's purpose explicitly, as
/// `crew-purpose:<purpose>`. An auto-task template carries it in its tags to
/// name a purpose for the tasks it files.
pub const CREW_PURPOSE_TAG_PREFIX: &str = "crew-purpose:";

/// One purpose of work whose crew can be drawn from its own pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrewPurpose {
    /// The task pilot activity.
    TaskPilot,
    /// Step-failure and PR conflict recovery.
    FailureRecovery,
    /// Blocked-task final recovery (`workflow.final_recovery_crews`).
    FinalRecovery,
    /// `orbit run auto --auto-fix` pull-request repairs.
    AutoFix,
    /// Automatic review (`operation.review_crew`).
    Review,
    /// Tasks the red-base re-check files (`baseline-red-repair`).
    BaselineRedRepair,
    /// Tasks the CI failure sweep files (`ci-failure-sweep`).
    CiFailureSweep,
    /// Merged-head repairs a review reconciliation files.
    MergedHeadRepair,
    /// Dependabot and code-scanning sweep tasks.
    Dependabot,
}

impl CrewPurpose {
    /// Every purpose, in the order `orbit config show` lists them.
    pub const ALL: [Self; 9] = [
        Self::TaskPilot,
        Self::FailureRecovery,
        Self::FinalRecovery,
        Self::AutoFix,
        Self::Review,
        Self::BaselineRedRepair,
        Self::CiFailureSweep,
        Self::MergedHeadRepair,
        Self::Dependabot,
    ];

    /// The purpose's stable name, as `workflow.purpose_crews.<name>` and the
    /// `crew-purpose:<name>` tag write it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskPilot => "task_pilot",
            Self::FailureRecovery => "failure_recovery",
            Self::FinalRecovery => "final_recovery",
            Self::AutoFix => "auto_fix",
            Self::Review => "review",
            Self::BaselineRedRepair => "baseline_red_repair",
            Self::CiFailureSweep => "ci_failure_sweep",
            Self::MergedHeadRepair => "merged_head_repair",
            Self::Dependabot => "dependabot",
        }
    }

    /// The purpose named `name`.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|purpose| purpose.as_str() == name.trim())
    }

    /// The configuration key holding this purpose's pool.
    pub fn config_key(self) -> &'static str {
        match self {
            Self::TaskPilot => "workflow.purpose_crews.task_pilot",
            Self::FailureRecovery => "workflow.purpose_crews.failure_recovery",
            Self::FinalRecovery => super::FINAL_RECOVERY_CREWS_KEY,
            Self::AutoFix => "workflow.purpose_crews.auto_fix",
            Self::Review => "operation.review_crew",
            Self::BaselineRedRepair => "workflow.purpose_crews.baseline_red_repair",
            Self::CiFailureSweep => "workflow.purpose_crews.ci_failure_sweep",
            Self::MergedHeadRepair => "workflow.purpose_crews.merged_head_repair",
            Self::Dependabot => "workflow.purpose_crews.dependabot",
        }
    }

    /// Whether the pool is a `workflow.purpose_crews.<purpose>` key, rather
    /// than a key that predates purposes.
    pub fn has_purpose_key(self) -> bool {
        self.config_key().starts_with(PURPOSE_CREWS_KEY_PREFIX)
    }

    /// The purpose whose `workflow.purpose_crews.<purpose>` key is `key`.
    pub fn from_purpose_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|purpose| purpose.has_purpose_key() && purpose.config_key() == key)
    }

    /// Whether this purpose chooses the crew of a task, rather than of an
    /// activity a run dispatches.
    pub fn is_task_purpose(self) -> bool {
        matches!(
            self,
            Self::BaselineRedRepair
                | Self::CiFailureSweep
                | Self::MergedHeadRepair
                | Self::Dependabot
        )
    }

    /// How the crew is chosen while the purpose's pool is unset: the route
    /// every host used before purposes existed. `None` for a purpose whose
    /// pool is its only route.
    pub fn fallback(self) -> Option<&'static str> {
        match self {
            Self::TaskPilot | Self::AutoFix => Some("workflow.low_complexity_crews"),
            Self::FailureRecovery => Some("workflow.medium_complexity_crews"),
            Self::FinalRecovery | Self::Review => None,
            Self::BaselineRedRepair
            | Self::CiFailureSweep
            | Self::MergedHeadRepair
            | Self::Dependabot => Some("the task's complexity pool, then workflow.default_crew"),
        }
    }
}

impl std::fmt::Display for CrewPurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
