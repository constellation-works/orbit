use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::logs::{LogReadBounds, RunLogRequests, read_run_log};

/// Default excerpt size. Small enough that a routine failed-step read costs an
/// executing agent a few thousand tokens rather than its whole context.
const DEFAULT_MAX_BYTES: u64 = 16_384;

/// Hard ceiling on one call's excerpt, regardless of what the caller asked for.
/// A single unbounded runner log can run to tens of megabytes; that is more
/// than any agent context can hold, so the tool refuses to return it.
const MAX_MAX_BYTES: u64 = 262_144;

pub struct GithubRunLogsTool;

impl GithubRunLogsTool {
    /// The `gh` requests this tool reads for `input` under `ctx`. Fallback
    /// job-log reads derive from these, so they share the same checkout.
    pub(crate) fn requests(
        &self,
        ctx: &crate::ToolContext,
        input: &Value,
    ) -> Result<RunLogRequests, OrbitError> {
        orbit_common::protocol::tool_input::reject_unknown_tool_fields(
            input,
            &[
                "run",
                "job",
                "scope",
                "max_bytes",
                "repo",
                "step",
                "byte_start",
                "byte_end",
                "line_start",
                "line_end",
            ],
        )?;
        let requests = RunLogRequests::from_input(input)?;
        Ok(match super::super::tool_workspace_dir(ctx) {
            Some(dir) => requests.in_directory(&dir),
            None => requests,
        })
    }
}

impl crate::Tool for GithubRunLogsTool {
    fn schema(&self) -> orbit_types::tool::ToolSchema {
        super::super::gh_schema(
            "github.run.logs",
            "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence. Optionally scope the excerpt to a named step (step, usually with job) or to a byte window (byte_start/byte_end, 0-based end-exclusive) or line window (line_start/line_end, 1-based end-exclusive); a window combined with step applies within the step section, an absent step or window fails without returning other log bytes, and the response reports the selection and its selected_bytes/selected_lines. The source stream is read incrementally with an 8 MiB stdout limit and process timeout. A separate diagnostic_unit retains a unique complete failing runner command up to 256 KiB; display truncation does not imply missing command evidence. Source-limit exhaustion is retryable. When the run-scoped read succeeds with no output, or GitHub says the parent run is still in progress, the excerpt can be recovered from the log API of a verified completed job belonging to that run; failed scope is restricted to unsuccessful jobs. `source` and `source_jobs` identify the recovery. Other errors remain errors.",
            vec![
                super::super::tool_param("run", "Numeric workflow-run ID", "string", true),
                super::super::tool_param(
                    "job",
                    "Numeric job ID to narrow the log to one job",
                    "string",
                    false,
                ),
                super::super::tool_param(
                    "scope",
                    "\"failed\" (default) for failed-step logs, or \"all\" for the full run log — use \"all\" to evidence the checked-out commit, since the checkout step usually succeeds",
                    "string",
                    false,
                ),
                super::super::tool_param(
                    "max_bytes",
                    "Maximum head/tail excerpt bytes (default 16384, capped at 262144), plus omission markers and up to 64 KiB of nextest failure blocks (16 KiB per block, at most 128 blocks). failure_regions includes original byte ranges; selection_complete is false if the scan or block-count limit is reached. The separate complete diagnostic_unit is capped at 262144 bytes.",
                    "integer",
                    false,
                ),
                super::super::tool_param(
                    "repo",
                    "Repository in owner/name format (uses current directory if omitted)",
                    "string",
                    false,
                ),
                super::super::tool_param(
                    "step",
                    "Exact workflow step name to return only that step's log section (usually with job); fails if the step is absent",
                    "string",
                    false,
                ),
                super::super::tool_param(
                    "byte_start",
                    "Start of a byte window (0-based, inclusive); with byte_end returns exactly those bytes, bounded by max_bytes",
                    "integer",
                    false,
                ),
                super::super::tool_param(
                    "byte_end",
                    "End of a byte window (0-based, exclusive); must be greater than byte_start",
                    "integer",
                    false,
                ),
                super::super::tool_param(
                    "line_start",
                    "Start of a line window (1-based, inclusive); with line_end returns exactly those lines, bounded by max_bytes",
                    "integer",
                    false,
                ),
                super::super::tool_param(
                    "line_end",
                    "End of a line window (1-based, exclusive); must be greater than line_start",
                    "integer",
                    false,
                ),
            ],
        )
    }

    fn execute(&self, ctx: &crate::ToolContext, input: Value) -> Result<Value, OrbitError> {
        let requests = self.requests(ctx, &input)?;
        let max_bytes =
            super::super::bounded_limit(&input, "max_bytes", DEFAULT_MAX_BYTES, MAX_MAX_BYTES)?
                as usize;
        let scope = requests.scope;
        let read = read_run_log(&requests, LogReadBounds::new(max_bytes), None)?;
        let log = read.log;
        let selection = log
            .selection
            .as_ref()
            .map(|selection| selection.describe(log.selected_bytes, log.selected_lines));
        let evidence = log.checkout_evidence;

        let mut output = json!({
            "run_id": input.get("run"),
            "scope": scope.as_str(),
            "log": log.text,
            "diagnostic_unit": log.diagnostic,
            "failure_regions": log.failure_regions,
            "source_complete": log.source_complete,
            "truncated": log.truncated,
            "returned_bytes": log.returned_bytes,
            "total_bytes": log.total_bytes,
            // Which query the excerpt above came from, and — when the
            // run-scoped read hit its empty/readiness blind spot — the job
            // whose own log stood in for it, so a reader is never left
            // guessing what these bytes describe.
            "source": read.source,
            "source_jobs": read.source_jobs,
            "fallback_error": read.fallback_error,
            // Distinct from any run's `reported_head_sha`: this is what the
            // runner checked out, read from the runner's own output.
            "checkout_commits": evidence.commits,
            "checkout_evidence": evidence.lines,
            "checkout_evidence_complete": evidence.complete,
            "checkout_evidence_scanned_bytes": evidence.scanned_bytes,
            "checkout_evidence_source_truncated": evidence.source_truncated,
        });
        if let Some(selection) = selection {
            output["selection"] = selection;
        }
        Ok(output)
    }
}
