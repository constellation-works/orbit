//! The script-step capability: the request the job executor hands its host
//! for one attempt of a `target: script:<name>` step, and the answer.
//!
//! The host owns the grant check, the snapshot and the sandboxed spawn; the
//! executor owns what the answer means for the step (its output, retry and
//! the `script_execution` audit entry).

use orbit_types::workflow::activity_job::ScriptExecutionRecord;
use serde_json::Value;

/// One attempt of a script step.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptStepRequest {
    pub run_id: String,
    pub step_id: String,
    /// The script name (`script:<name>` without its prefix).
    pub script: String,
    /// The rendered step input: validated against the manifest's
    /// `input_schema_json`, rendered into its `args` and written to stdin.
    pub input: Value,
    /// The run's worktree (`workspace_path` of the run input), when the job
    /// has one; otherwise the script runs in the workspace root, read-only.
    pub workspace_path: Option<String>,
    /// The step's `timeout_seconds`; the manifest's request or the default
    /// applies when absent, capped by the grant.
    pub timeout_seconds: Option<u64>,
}

/// How a script that ran ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptStepEnd {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    ScratchCapExceeded,
}

/// What the script produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptStepRan {
    pub end: ScriptStepEnd,
    pub stdout: Vec<u8>,
    pub stdout_over_cap: bool,
    /// The snapshotted manifest's `output_schema_json` (`null` for none).
    pub output_schema: Value,
    /// The capped, redacted log of stdout and stderr.
    pub log: Vec<u8>,
    pub record: ScriptExecutionRecord,
}

/// The host's answer for one attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptStepRun {
    /// Nothing was spawned. `code` is one of
    /// `orbit_types::workflow::activity_job::script_refusal`.
    Refused {
        code: &'static str,
        message: String,
        record: Box<ScriptExecutionRecord>,
    },
    Ran(Box<ScriptStepRan>),
}
