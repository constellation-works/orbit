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
}

impl ReadinessGap {
    pub fn is_blocking(&self) -> bool {
        self.severity == ReadinessSeverity::Blocking
    }

    /// `{code, severity, message, fix}`.
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code.as_str(),
            "severity": self.severity.as_str(),
            "message": self.message,
            "fix": self.fix,
        })
    }
}

const CONTEXT_FILES_FIX: &str =
    "add context files, run the task pilot, or tag no-diff-expected if no diff is expected";
const COMPLEXITY_FIX: &str = "set --complexity, or let the task pilot assess it";

/// The gaps of a task at `stage`, in the order drain approval reports them:
/// missing context files, then unassessed complexity.
///
/// A [`NO_DIFF_EXPECTED_TAG`] task has none: it has no modification targets to
/// declare or size. Otherwise a task needs context selectors (blocking when
/// proposed, advisory in the backlog, which admission does not check) and an
/// assessed complexity (blocking at both stages).
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
    if context_files.is_empty() {
        gaps.push(match stage {
            ReadinessStage::Proposed => ReadinessGap {
                code: ReadinessGapCode::MissingContextFiles,
                severity: ReadinessSeverity::Blocking,
                message: "No context files are declared, so a drain will not auto-approve it.",
                fix: CONTEXT_FILES_FIX,
            },
            ReadinessStage::Backlog => ReadinessGap {
                code: ReadinessGapCode::MissingContextFiles,
                severity: ReadinessSeverity::Advisory,
                message: "No context files are declared; it can still run, but starts cold.",
                fix: CONTEXT_FILES_FIX,
            },
        });
    }
    if !complexity.is_some_and(TaskComplexity::is_assessed) {
        gaps.push(ReadinessGap {
            code: ReadinessGapCode::UnassessedComplexity,
            severity: ReadinessSeverity::Blocking,
            message: match stage {
                ReadinessStage::Proposed => {
                    "Complexity is unassessed, so a drain will not auto-approve it."
                }
                ReadinessStage::Backlog => {
                    "Complexity is unassessed, so automation will not admit it."
                }
            },
            fix: COMPLEXITY_FIX,
        });
    }
    gaps
}

/// A `proposed` or `backlog` task's readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReadiness {
    pub gaps: Vec<ReadinessGap>,
}

impl TaskReadiness {
    /// Ready means no blocking gap; advisory gaps do not hold a task.
    pub fn ready(&self) -> bool {
        !self.gaps.iter().any(ReadinessGap::is_blocking)
    }

    /// `{ready, gaps: [{code, severity, message, fix}]}`.
    pub fn to_json(&self) -> Value {
        json!({
            "ready": self.ready(),
            "gaps": self.gaps.iter().map(ReadinessGap::to_json).collect::<Vec<_>>(),
        })
    }
}

/// The task's readiness, or `None` for a status other than `proposed` or
/// `backlog`.
pub fn task_readiness(task: &Task) -> Option<TaskReadiness> {
    ReadinessStage::of(task.status).map(|stage| TaskReadiness {
        gaps: readiness_gaps(stage, &task.tags, &task.context_files, task.complexity),
    })
}

/// The `readiness` enrichment of a task readout; `None` (the key omitted) for
/// a status readiness does not describe.
pub fn task_readiness_json(task: &Task) -> Option<Value> {
    task_readiness(task).map(|readiness| readiness.to_json())
}
