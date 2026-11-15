//! Task readiness: what a `proposed` or `backlog` task still lacks before
//! automation will approve or admit it.
//!
//! [`readiness_gaps`] is the only implementation of the context-files and
//! complexity rules. Drain approval (`run auto --approve-proposed`) withholds a
//! `proposed` task on its first blocking gap, backlog admission withholds a
//! task with any blocking gap, and every read surface (task show, the task
//! write tools, the dashboard) projects the same gaps, so what a reader is told
//! a task lacks is exactly what automation checks.
//!
//! A backlog task with no footprint ([`declares_no_footprint`]) is not
//! refused, but it holds no file lock: a multi-slot drain holds it for the
//! task pilot and then runs it alone [ORB-15191]. Its advisory gap and the
//! task write warning ([`backlog_footprint_warning`]) say so.
//!
//! An operator may waive a backlog task's waivable gaps with the
//! [`READINESS_WAIVED_TAG`] tag [ORB-25084]: missing context files (the
//! unknown-footprint rule) and unassessed complexity. A waived gap is still
//! reported, as advisory and `waived`, and automatic selection admits the task
//! like ordinary ready work: it holds no file lock and does not reserve the
//! tree, so a drain may run it beside other work. Waits outside readiness
//! (host OS, labels, browser kit, human crew, dependencies, real lock holders)
//! are not readiness gaps, so the tag never lifts them. The proposed stage is
//! not waivable: approval still needs a prepared task.
//!
//! Readiness covers what an edit to the task fixes. Waits that clear on their
//! own — dependencies, context locks, host OS or crew, provider backoff, a red
//! base — are reported by the drain, not here. A `no-auto-approve` tag is an
//! operator choice, not a gap.

use serde_json::{Value, json};

use super::{NO_DIFF_EXPECTED_TAG, Task, TaskComplexity, TaskStatus};

/// The lifecycle stage readiness is judged against. Only `proposed` and
/// `backlog` tasks wait on preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessStage {
    /// Awaiting approval; drain approval checks every gap.
    Proposed,
    /// Approved and queued; admission checks only the complexity gap.
    Backlog,
}

impl ReadinessStage {
    /// The stage for `status`, or `None` for a status readiness does not
    /// describe.
    pub fn of(status: TaskStatus) -> Option<Self> {
        match status {
            TaskStatus::Proposed => Some(Self::Proposed),
            TaskStatus::Backlog => Some(Self::Backlog),
            _ => None,
        }
    }
}

/// What a task lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadinessGapCode {
    /// No `context_files`, and the task is expected to produce a diff.
    MissingContextFiles,
    /// Complexity is `unassessed` or absent, and the task is expected to
    /// produce a diff.
    UnassessedComplexity,
}

impl ReadinessGapCode {
    /// The wire code; also the drain's hold reason for the same gap.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingContextFiles => "missing_context_files",
            Self::UnassessedComplexity => "unassessed_complexity",
        }
    }
}

/// Whether automation refuses the task while the gap stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadinessSeverity {
    /// Automation will not approve or admit the task.
    Blocking,
    /// Automation proceeds, but the run starts with less to go on.
    Advisory,
}

impl ReadinessSeverity {
    /// The wire value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Advisory => "advisory",
        }
    }
}

/// One thing a task lacks, with what it costs and how to supply it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessGap {
    pub code: ReadinessGapCode,
    pub severity: ReadinessSeverity,
    /// What the gap means for this task, in one sentence.
    pub message: &'static str,
    /// How to close it.
    pub fix: &'static str,
    /// An operator waived it with [`READINESS_WAIVED_TAG`]; it is then
    /// advisory, and automation admits the task despite it.
    pub waived: bool,
}

impl ReadinessGap {
    pub fn is_blocking(&self) -> bool {
        self.severity == ReadinessSeverity::Blocking
    }

    /// `{code, severity, message, fix}`, plus `waived: true` for a waived
    /// gap.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "code": self.code.as_str(),
            "severity": self.severity.as_str(),
            "message": self.message,
            "fix": self.fix,
        });
        if self.waived {
            value["waived"] = Value::Bool(true);
        }
        value
    }
}

/// The tag an operator adds to waive a backlog task's readiness gaps, so
/// automatic drain selection admits it like ordinary ready work.
pub const READINESS_WAIVED_TAG: &str = "readiness-waived";

/// Whether `tags` carry the operator's readiness waiver.
pub fn waives_readiness(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == READINESS_WAIVED_TAG)
}

/// The gaps an operator can waive at `stage`: every backlog gap, no proposed
/// one. Host and crew waits are not readiness gaps, so never waivable.
pub fn is_waivable(stage: ReadinessStage, _code: ReadinessGapCode) -> bool {
    stage == ReadinessStage::Backlog
}

const CONTEXT_FILES_FIX: &str =
    "add context files, run the task pilot, or tag no-diff-expected if no diff is expected";

/// Whether a task declares no footprint: no `context_files`, and no
/// [`NO_DIFF_EXPECTED_TAG`] saying it edits nothing. Such a task holds no
/// file lock, so it can conflict with any concurrent work.
pub fn declares_no_footprint(tags: &[String], context_files: &[String]) -> bool {
    context_files.is_empty() && !tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG)
}

/// Whether multi-slot admission applies the unknown-footprint rule to a
/// task: it declares no footprint ([`declares_no_footprint`]) and no operator
/// waived that rule ([`READINESS_WAIVED_TAG`]). A waived task holds no file
/// lock and reserves nothing, so it waits for no pilot or exclusive slot and
/// holds no whole tree while it runs.
pub fn footprint_rule_applies(tags: &[String], context_files: &[String]) -> bool {
    declares_no_footprint(tags, context_files) && !waives_readiness(tags)
}
const COMPLEXITY_FIX: &str = "set --complexity, or let the task pilot assess it";

/// The gaps of a task at `stage`, in the order drain approval reports them:
/// missing context files, then unassessed complexity.
///
/// A [`NO_DIFF_EXPECTED_TAG`] task has none: it has no modification targets to
/// declare or size. Otherwise a task needs context selectors (blocking when
/// proposed; advisory in the backlog, where a multi-slot drain holds the task
/// for a footprint rather than refusing it) and an assessed complexity
/// (blocking at both stages).
pub fn readiness_gaps(
    stage: ReadinessStage,
    tags: &[String],
    context_files: &[String],
    complexity: Option<TaskComplexity>,
) -> Vec<ReadinessGap> {
    let mut gaps = Vec::new();
    if tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG) {
        return gaps;
    }
    let waived = stage == ReadinessStage::Backlog && waives_readiness(tags);
    if declares_no_footprint(tags, context_files) {
        gaps.push(match (stage, waived) {
            (ReadinessStage::Proposed, _) => ReadinessGap {
                code: ReadinessGapCode::MissingContextFiles,
                severity: ReadinessSeverity::Blocking,
                message: "No context files are declared, so a drain will not auto-approve it.",
                fix: CONTEXT_FILES_FIX,
                waived: false,
            },
            (ReadinessStage::Backlog, false) => ReadinessGap {
                code: ReadinessGapCode::MissingContextFiles,
                severity: ReadinessSeverity::Advisory,
                message: "No context files are declared, so it holds no file lock and can \
                          conflict with concurrent work; a multi-slot drain waits for the task \
                          pilot to prepare it, then runs it only alone.",
                fix: CONTEXT_FILES_FIX,
                waived: false,
            },
            (ReadinessStage::Backlog, true) => ReadinessGap {
                code: ReadinessGapCode::MissingContextFiles,
                severity: ReadinessSeverity::Advisory,
                message: "No context files are declared and an operator waived the footprint \
                          rule, so it holds no file lock and a drain may run it beside \
                          concurrent work it can conflict with.",
                fix: CONTEXT_FILES_FIX,
                waived: true,
            },
        });
    }
    if !complexity.is_some_and(TaskComplexity::is_assessed) {
        gaps.push(ReadinessGap {
            code: ReadinessGapCode::UnassessedComplexity,
            severity: if waived {
                ReadinessSeverity::Advisory
            } else {
                ReadinessSeverity::Blocking
            },
            message: match (stage, waived) {
                (ReadinessStage::Proposed, _) => {
                    "Complexity is unassessed, so a drain will not auto-approve it."
                }
                (ReadinessStage::Backlog, false) => {
                    "Complexity is unassessed, so automation will not admit it."
                }
                (ReadinessStage::Backlog, true) => {
                    "Complexity is unassessed and an operator waived the gate, so automation \
                     admits it with its configured or default crew."
                }
            },
            fix: COMPLEXITY_FIX,
            waived,
        });
    }
    gaps
}

/// A `proposed` or `backlog` task's readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReadiness {
    pub stage: ReadinessStage,
    pub gaps: Vec<ReadinessGap>,
}

impl TaskReadiness {
    /// Ready means no blocking gap; advisory gaps do not hold a task.
    pub fn ready(&self) -> bool {
        !self.gaps.iter().any(ReadinessGap::is_blocking)
    }

    /// The codes of the gaps an operator can still waive: unwaived gaps the
    /// stage lets [`READINESS_WAIVED_TAG`] waive.
    pub fn waivable(&self) -> Vec<ReadinessGapCode> {
        self.gaps
            .iter()
            .filter(|gap| !gap.waived && is_waivable(self.stage, gap.code))
            .map(|gap| gap.code)
            .collect()
    }

    /// `{ready, gaps: [{code, severity, message, fix, waived?}]}`, plus
    /// `waivable: [code]` while an operator can still waive a gap.
    pub fn to_json(&self) -> Value {
        let mut value = json!({
            "ready": self.ready(),
            "gaps": self.gaps.iter().map(ReadinessGap::to_json).collect::<Vec<_>>(),
        });
        let waivable = self.waivable();
        if !waivable.is_empty() {
            value["waivable"] = json!(
                waivable
                    .iter()
                    .map(|code| code.as_str())
                    .collect::<Vec<_>>()
            );
        }
        value
    }
}

/// The task's readiness, or `None` for a status other than `proposed` or
/// `backlog`.
pub fn task_readiness(task: &Task) -> Option<TaskReadiness> {
    ReadinessStage::of(task.status).map(|stage| TaskReadiness {
        stage,
        gaps: readiness_gaps(stage, &task.tags, &task.context_files, task.complexity),
    })
}

/// The `readiness` enrichment of a task readout; `None` (the key omitted) for
/// a status readiness does not describe.
pub fn task_readiness_json(task: &Task) -> Option<Value> {
    task_readiness(task).map(|readiness| readiness.to_json())
}

/// The warning a task write returns when it leaves the task in `backlog` with
/// no footprint: the backlog gap's message and fix, so the writer reads the
/// same words as readiness, waived or not. `None` for every other task.
pub fn backlog_footprint_warning(task: &Task) -> Option<String> {
    if task.status != TaskStatus::Backlog {
        return None;
    }
    readiness_gaps(
        ReadinessStage::Backlog,
        &task.tags,
        &task.context_files,
        task.complexity,
    )
    .into_iter()
    .find(|gap| gap.code == ReadinessGapCode::MissingContextFiles)
    .map(|gap| format!("{} Fix: {}.", gap.message, gap.fix))
}
