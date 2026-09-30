//! Scoreboard summary: the per-agent delivery rollup written to
//! `summary.json`.
//!
//! `types` holds the summary document and its inputs, `generate` builds it,
//! `overlay` folds audit, friction and scoreboard metrics into agent rows,
//! `files` reads and writes the scoreboard files, and `highlights` selects
//! notable completions and coverage notes.

mod files;
mod generate;
mod highlights;
mod overlay;
mod types;

pub use files::write_summary;
pub use generate::generate_summary_with_inputs;
#[cfg(test)]
pub use generate::{generate_summary, generate_summary_with_audit_tool_calls};
pub use highlights::{
    NotableCompletions, ScoreboardCoverage, fill_notable_summary_excerpts,
    select_notable_completions, snapshot_coverage,
};
pub use types::{
    AgentSummary, NormalizedTokenSummary, ORCHESTRATION_SCHEMA_VERSION, OrchestrationBucketKind,
    OrchestrationBucketSummary, OrchestrationModelSummary, OrchestrationSummary, RecentSummary,
    ScoreboardInputs, ScoreboardSummary, ScoreboardWindow, TopToolCall, WorkflowRunCount,
};

#[cfg(test)]
mod tests;
