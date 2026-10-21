use super::*;

/// Tasks carrying this tag may complete successfully without producing a
/// repository diff. Their durable side effects live outside git (for example,
/// QA validation tasks file follow-up Orbit tasks).
pub const NO_DIFF_EXPECTED_TAG: &str = "no-diff-expected";

/// Tasks carrying this tag are never approved from `proposed` into `backlog`
/// by an automatic promotion authority (an `--approve-proposed` drain or the
/// CI sweep); they stay `proposed`, held with this tag as the reason, until a
/// human approves them.
pub const NO_AUTO_APPROVE_TAG: &str = "no-auto-approve";

/// Task history event an operator records to discard the candidate the task's
/// last failed run preserved; the next run implements fresh [ORB-13985].
pub const CANDIDATE_DISCARDED_EVENT: &str = "candidate_discarded";

/// Task history event recording what a run, or a claim's admission, did with
/// the candidate an earlier run or claim of the task preserved [ORB-13985]
/// [ORB-14338].
pub const CANDIDATE_RESUME_EVENT: &str = "candidate_resume";

/// Task history event the owner records when a claim's failure or release
/// keeps the leaf's committed candidate: its commit, the ref and pull request
/// it is on, and the run and machine that made it. The claim settlement's own
/// record stays the one admission and `orbit task requeue` read; this entry
/// shows on the task what the next run continues.
pub const CLAIM_CANDIDATE_KEPT_EVENT: &str = "claim_candidate_kept";

/// Task history event recorded when Orbit, not the executing agent, wrote the
/// task's execution summary. It is audit history and stays after the agent
/// replaces the summary; [`DERIVED_EXECUTION_SUMMARY_PREFIX`] tells whether the
/// current text is still Orbit's [ORB-15186].
pub const EXECUTION_SUMMARY_DERIVED_EVENT: &str = "execution_summary_derived";

/// First words of every execution summary Orbit derives. The history event
/// outlives a later agent-authored replacement, so only the current summary's
/// text says who wrote it [ORB-15255].
pub const DERIVED_EXECUTION_SUMMARY_PREFIX: &str = "Execution summary derived by Orbit for";

/// Operator-facing projection for a valid task reference whose prefix is not
/// represented in this machine's coordination registry.
pub const TASK_REFERENCE_NOT_VERIFIABLE_HERE: &str = "not verifiable here";

/// Default maximum number of tasks a task listing returns (ORB-10310). The
/// `orbit task list` CLI and `orbit.task.list` MCP tool return at most the
/// newest `DEFAULT_TASK_LIST_LIMIT` matching tasks, with status-aware ordering
/// when no lifecycle status filter is supplied.
pub const DEFAULT_TASK_LIST_LIMIT: usize = 50;

/// Named bucket for an optional indexed label that nobody set.
///
/// Aggregates must keep this visible rather than dropping the row or folding it
/// into a populated band — the unlabeled set is often the largest bucket
/// (ORB-10889 / ORB-10891).
pub const UNSET_BUCKET: &str = "unset";

/// Map an optional indexed label onto a display bucket.
///
/// Empty or missing values become [`UNSET_BUCKET`]. A present non-empty value
/// is returned unchanged so unexpected labels stay their own band instead of
/// being merged into a known one.
pub fn labeled_or_unset(value: Option<&str>) -> &str {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(label) => label,
        None => UNSET_BUCKET,
    }
}

/// Map an indexed complexity label onto a display bucket.
///
/// [`TaskComplexity::Unassessed`] is the stored spelling of "nobody assessed
/// this" ([`TaskComplexity::is_assessed`] is `false`), and rows indexed before
/// the column existed spell the same thing as `NULL`. Reporting surfaces get
/// one bucket for that concept — [`UNSET_BUCKET`] — rather than splitting the
/// unlabeled set across `unset` and `unassessed` (ORB-10895). The fold happens
/// here, at read time: the persisted and indexed value stays the literal
/// `unassessed` so provenance and `NULL`-backfill detection remain honest.
pub fn complexity_bucket(value: Option<&str>) -> &str {
    let label = labeled_or_unset(value);
    if label == TaskComplexity::Unassessed.as_str() {
        UNSET_BUCKET
    } else {
        label
    }
}

/// Stable display order: `unset`, then `low` / `medium` / `hard` / `xhard`,
/// then any unexpected label alphabetically.
pub fn complexity_bucket_ord(label: &str) -> (u8, &str) {
    match label {
        UNSET_BUCKET => (0, ""),
        "low" => (1, ""),
        "medium" => (2, ""),
        "hard" => (3, ""),
        "xhard" => (4, ""),
        other => (5, other),
    }
}

/// Current lifecycle state of a task.
///
/// See the module-level documentation for status-edit semantics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Awaiting human approval before entering the backlog.
    Proposed,
    /// Approved and queued for work; not yet started.
    Backlog,
    /// Actively being worked on.
    #[cfg_attr(feature = "clap", value(name = "in-progress", alias = "in_progress"))]
    #[serde(alias = "in-progress")]
    InProgress,
    /// Implementation complete; awaiting review/merge.
    Review,
    /// Accepted and closed. May be reopened by an explicit status edit.
    Done,
    /// Temporarily paused (waiting on a dependency or decision).
    Blocked,
    /// Soft-deleted. Can be restored to any other status.
    Archived,
    /// Declined. Can be reclassified to any other status.
    Rejected,
    /// Future-scoped — wanted but not yet actionable. Agents skip someday tasks.
    Someday,
}

/// Lifecycle states that may be selected when a task is first created.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskCreateStatus {
    /// Awaiting human approval before entering the backlog.
    Proposed,
    /// Approved and queued for work; not yet started.
    Backlog,
    /// Future-scoped — wanted but not yet actionable.
    Someday,
}

impl From<TaskCreateStatus> for TaskStatus {
    fn from(value: TaskCreateStatus) -> Self {
        match value {
            TaskCreateStatus::Proposed => Self::Proposed,
            TaskCreateStatus::Backlog => Self::Backlog,
            TaskCreateStatus::Someday => Self::Someday,
        }
    }
}

impl Display for TaskStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cli_name())
    }
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "proposed" => Ok(TaskStatus::Proposed),
            "backlog" => Ok(TaskStatus::Backlog),
            "in-progress" => Ok(TaskStatus::InProgress),
            "in_progress" => Ok(TaskStatus::InProgress),
            "review" => Ok(TaskStatus::Review),
            "done" => Ok(TaskStatus::Done),
            "blocked" => Ok(TaskStatus::Blocked),
            "archived" => Ok(TaskStatus::Archived),
            "rejected" => Ok(TaskStatus::Rejected),
            "someday" => Ok(TaskStatus::Someday),
            other => Err(format!("unknown task status: {other}")),
        }
    }
}

impl TaskStatus {
    /// Every status, in declaration order.
    pub const ALL: [TaskStatus; 9] = [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Blocked,
        TaskStatus::Archived,
        TaskStatus::Rejected,
        TaskStatus::Someday,
    ];

    pub fn cli_name(self) -> &'static str {
        match self {
            TaskStatus::Proposed => "proposed",
            TaskStatus::Backlog => "backlog",
            TaskStatus::InProgress => "in-progress",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Archived => "archived",
            TaskStatus::Rejected => "rejected",
            TaskStatus::Someday => "someday",
        }
    }

    /// Returns true for the statuses that settle a task: `done`, `archived`
    /// and `rejected`.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Done | TaskStatus::Archived | TaskStatus::Rejected
        )
    }

    /// Returns true when the status satisfies a task dependency.
    ///
    /// Orbit's lifecycle has no standalone terminal `approved` status for
    /// tasks today; accepted work lands in `done`.
    pub fn satisfies_dependency(self) -> bool {
        matches!(self, TaskStatus::Done)
    }

    /// Classifies a *non*-satisfying status as either a legitimate wait or a
    /// dead end.
    ///
    /// `satisfies_dependency` answers "is this edge satisfied now?"; this
    /// answers the complementary "could waiting ever satisfy it?". A
    /// dependency in `backlog` / `proposed` / `in-progress` / `review` /
    /// `blocked` / `someday` returns `None` — it can still reach `done`, so
    /// callers must keep waiting. `archived` and `rejected` return `Some`:
    /// both are restorable, but only by an operator editing the task graph,
    /// never by the passage of time.
    ///
    /// This deliberately does *not* widen what counts as satisfied
    /// (`Done`-only stays) — it only lets dispatch fail loudly instead of
    /// polling out its whole budget against an edge that cannot close. An
    /// archived target that reached `done` before its archive is projected as
    /// `done` before this is consulted
    /// (`satisfy_completed_archived_dependencies`), so it never lands here.
    pub fn dependency_dead_end(self) -> Option<DependencyDeadEnd> {
        match self {
            TaskStatus::Archived => Some(DependencyDeadEnd::Archived),
            TaskStatus::Rejected => Some(DependencyDeadEnd::Rejected),
            TaskStatus::Proposed
            | TaskStatus::Backlog
            | TaskStatus::InProgress
            | TaskStatus::Review
            | TaskStatus::Done
            | TaskStatus::Blocked
            | TaskStatus::Someday => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

impl Display for TaskPriority {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TaskPriority::Low => "low",
            TaskPriority::Medium => "medium",
            TaskPriority::High => "high",
            TaskPriority::Critical => "critical",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TaskPriority {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(TaskPriority::Low),
            "medium" => Ok(TaskPriority::Medium),
            "high" => Ok(TaskPriority::High),
            "critical" => Ok(TaskPriority::Critical),
            other => Err(format!("unknown task priority: {other}")),
        }
    }
}

/// How much work a task is expected to take.
///
/// `Low` / `Medium` / `Hard` / `XHard` are operator assessments, in ascending
/// order of expected effort. This scale is deliberately distinct from the
/// provider effort scale in `identity::agent_pair`. `Unassessed` is the
/// explicit non-answer for automated creation (auto-task mint, unlabeled
/// import). It is never a silent stand-in for `Medium`. Human and agent
/// create *and update* surfaces accept only assessed values, so an agent
/// cannot satisfy the create assessment and clear it on the next update.
/// Persisted `Task.complexity` stays `Option` so the historical unlabeled set
/// remains `None`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Low,
    Medium,
    Hard,
    /// Reserved top tier for the most capable (and most expensive) crews.
    // Keep this spelling aligned across serde and clap; their default naming
    // conventions would otherwise split it into `x_hard` and `x-hard`.
    #[serde(rename = "xhard")]
    #[cfg_attr(feature = "clap", value(name = "xhard"))]
    XHard,
    /// Explicit non-answer for automated creation. Not offered on CLI
    /// `--complexity` (clap skips it) and rejected on human/agent create and
    /// update surfaces so agents cannot dodge an assessment.
    #[cfg_attr(feature = "clap", value(skip))]
    Unassessed,
}

impl Display for TaskComplexity {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for TaskComplexity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(TaskComplexity::Low),
            "medium" => Ok(TaskComplexity::Medium),
            "hard" => Ok(TaskComplexity::Hard),
            "xhard" => Ok(TaskComplexity::XHard),
            "unassessed" => Ok(TaskComplexity::Unassessed),
            other => Err(format!("unknown task complexity: {other}")),
        }
    }
}

impl TaskComplexity {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskComplexity::Low => "low",
            TaskComplexity::Medium => "medium",
            TaskComplexity::Hard => "hard",
            TaskComplexity::XHard => "xhard",
            TaskComplexity::Unassessed => "unassessed",
        }
    }

    /// `low` / `medium` / `hard` / `xhard` — values an operator or agent may
    /// assign.
    pub fn is_assessed(self) -> bool {
        !matches!(self, TaskComplexity::Unassessed)
    }

    /// Reject [`TaskComplexity::Unassessed`] on human/agent create and update
    /// surfaces.
    pub fn require_assessed(self) -> Result<Self, TaskError> {
        if self.is_assessed() {
            Ok(self)
        } else {
            Err(TaskError::UnassessedComplexity)
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Feature,
    /// An attributable defect; lineage is recorded through typed task relations.
    Bug,
    Refactor,
    Chore,
}

impl Display for TaskType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TaskType::Feature => "feature",
            TaskType::Bug => "bug",
            TaskType::Refactor => "refactor",
            TaskType::Chore => "chore",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TaskType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "feature" => Ok(TaskType::Feature),
            "bug" => Ok(TaskType::Bug),
            "refactor" => Ok(TaskType::Refactor),
            "chore" => Ok(TaskType::Chore),
            other => Err(format!(
                "unknown task type: {other} (valid types: {})",
                TaskType::valid_names().join(", ")
            )),
        }
    }
}

impl TaskType {
    pub fn valid_names() -> &'static [&'static str] {
        &["feature", "bug", "refactor", "chore"]
    }
}

/// Why a task was filed: a human operator asked for it, an orchestrator
/// decided on its own, or automation minted it.
///
/// Persisted `Task.origin` stays `Option`: a task filed before origins were
/// recorded, or by an interactive caller that did not say, has none, and
/// that absence is never read as any of these values.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum TaskOrigin {
    /// A human operator asked for the work.
    #[cfg_attr(feature = "clap", value(name = "operator_request"))]
    OperatorRequest,
    /// An orchestrator judged the work necessary on its own initiative.
    #[cfg_attr(feature = "clap", value(name = "orchestrator_initiative"))]
    OrchestratorInitiative,
    /// A system filer minted it: an auto-task definition, routine, sweep or
    /// other automated creation path.
    Automation,
}

impl TaskOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskOrigin::OperatorRequest => "operator_request",
            TaskOrigin::OrchestratorInitiative => "orchestrator_initiative",
            TaskOrigin::Automation => "automation",
        }
    }

    pub fn valid_names() -> &'static [&'static str] {
        &["operator_request", "orchestrator_initiative", "automation"]
    }
}

impl Display for TaskOrigin {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for TaskOrigin {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "operator_request" => Ok(TaskOrigin::OperatorRequest),
            "orchestrator_initiative" => Ok(TaskOrigin::OrchestratorInitiative),
            "automation" => Ok(TaskOrigin::Automation),
            other => Err(format!(
                "unknown task origin: {other} (valid origins: {})",
                TaskOrigin::valid_names().join(", ")
            )),
        }
    }
}

/// Which rule supplied a task's `orchestrator_model` when it was created.
///
/// Persisted `Task.orchestrator_model_source` stays `Option`: a task without
/// an orchestrator model, including every task filed before the field
/// existed, has none.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorModelSource {
    /// The creating call named the model explicitly.
    Explicit,
    /// Snapshotted from the orchestrator crew's configured model.
    CrewConfig,
}

impl OrchestratorModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OrchestratorModelSource::Explicit => "explicit",
            OrchestratorModelSource::CrewConfig => "crew_config",
        }
    }
}

impl Display for OrchestratorModelSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
