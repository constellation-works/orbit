//! The purposes a crew is chosen for by a pool of its own.
//!
//! Each purpose names one weighted pool, written like the complexity pools.
//! An explicit crew always wins over it, and a purpose whose pool is unset
//! resolves exactly as it did before purposes existed: [`CrewPurpose::fallback`]
//! names that route.
//!
//! Every purpose reads `workflow.purpose_crews.<purpose>`. Final recovery and
//! automatic review had pools of their own before purposes existed, and they
//! keep those pools' meaning under their purpose key: no fallback route, and
//! review's draw avoids the implementer's crew.
//!
//! The CI failure, Dependabot and code-scanning sweeps and merged-head repairs
//! share one purpose, [`CrewPurpose::SweepTasks`]. Tasks recorded their
//! purpose by name, so [`CrewPurpose::parse`] still reads the names those
//! purposes had on their own [ORB-16775].

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
    /// Blocked-task final recovery.
    FinalRecovery,
    /// `orbit run auto --auto-fix` pull-request repairs.
    AutoFix,
    /// Automatic review.
    Review,
    /// Tasks the red-base re-check files (`baseline-red-repair`).
    BaselineRedRepair,
    /// Tasks the CI failure, Dependabot and code-scanning sweeps file, and
    /// merged-head repairs a review reconciliation files.
    SweepTasks,
    /// Read-only exploration an implement or review-repair run delegates
    /// through `orbit.agent.invoke`.
    Explorer,
}

/// Names [`CrewPurpose::SweepTasks`] had as three purposes, which tasks
/// filed before they merged still record.
const LEGACY_SWEEP_PURPOSE_NAMES: &[&str] =
    &["ci_failure_sweep", "merged_head_repair", "dependabot"];

impl CrewPurpose {
    /// Every purpose, in the order `orbit config show` lists them.
    pub const ALL: [Self; 8] = [
        Self::TaskPilot,
        Self::FailureRecovery,
        Self::FinalRecovery,
        Self::AutoFix,
        Self::Review,
        Self::BaselineRedRepair,
        Self::SweepTasks,
        Self::Explorer,
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
            Self::SweepTasks => "sweep_tasks",
            Self::Explorer => "explorer",
        }
    }

    /// The purpose named `name`, including a sweep purpose's former name.
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim();
        if LEGACY_SWEEP_PURPOSE_NAMES.contains(&name) {
            return Some(Self::SweepTasks);
        }
        Self::ALL
            .into_iter()
            .find(|purpose| purpose.as_str() == name)
    }

    /// The configuration key holding this purpose's pool.
    pub fn config_key(self) -> &'static str {
        match self {
            Self::TaskPilot => "workflow.purpose_crews.task_pilot",
            Self::FailureRecovery => "workflow.purpose_crews.failure_recovery",
            Self::FinalRecovery => super::FINAL_RECOVERY_CREWS_CONFIG_KEY,
            Self::AutoFix => "workflow.purpose_crews.auto_fix",
            Self::Review => "workflow.purpose_crews.review",
            Self::BaselineRedRepair => "workflow.purpose_crews.baseline_red_repair",
            Self::SweepTasks => "workflow.purpose_crews.sweep_tasks",
            Self::Explorer => "workflow.purpose_crews.explorer",
        }
    }

    /// Whether the purpose draws through the shared purpose-pool route, with
    /// a fallback while its pool is unset. Final recovery and review keep the
    /// draws their pools had before purposes existed.
    pub fn has_purpose_key(self) -> bool {
        !matches!(self, Self::FinalRecovery | Self::Review)
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
        matches!(self, Self::BaselineRedRepair | Self::SweepTasks)
    }

    /// How the crew is chosen while the purpose's pool is unset: the route
    /// every host used before purposes existed. `None` for a purpose whose
    /// pool is its only route.
    pub fn fallback(self) -> Option<&'static str> {
        match self {
            Self::TaskPilot | Self::AutoFix => Some("workflow.low_complexity_crews"),
            Self::FailureRecovery => Some("workflow.medium_complexity_crews"),
            // An explorer is a cheap crew by intent; with no pool there is no
            // explorer, and agent callers are refused.
            Self::FinalRecovery | Self::Review | Self::Explorer => None,
            Self::BaselineRedRepair | Self::SweepTasks => {
                Some("the task's complexity pool, then workflow.default_crew")
            }
        }
    }
}

impl std::fmt::Display for CrewPurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
