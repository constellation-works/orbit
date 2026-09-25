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

pub use files::{summary_path, write_summary};
pub use generate::{
    generate_summary, generate_summary_with_audit_tool_calls, generate_summary_with_inputs,
};
pub use highlights::{
    CoverageAvailability, CoverageNote, NotableCompletion, NotableCompletions, ScoreboardCoverage,
    select_notable_completions, snapshot_coverage,
};
pub use types::{
    AgentSummary, FrictionSummary, NormalizedTokenSummary, ORCHESTRATION_SCHEMA_VERSION,
    OrchestrationBucketKind, OrchestrationBucketSummary, OrchestrationModelSummary,
    OrchestrationSummary, PrSummary, RecentSummary, ScoreboardInputs, ScoreboardSummary,
    ScoreboardWindow, TokenSummary, TopToolCall, WorkflowRunCount,
};

#[cfg(test)]
mod tests;
