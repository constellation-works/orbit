use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::logs::{LogReadBounds, RunLogRequests, read_run_log};

/// Default excerpt size. Small enough that a routine failed-step read costs an
/// executing agent a few thousand tokens rather than its whole context.
const DEFAULT_MAX_BYTES: u64 = 16_384;

/// Hard ceiling on one call's excerpt, regardless of what the caller asked for.
/// A single unbounded runner log can run to tens of megabytes; that is more
/// than any agent context can hold, so the tool refuses to return it.
const MAX_MAX_BYTES: u64 = 262_144;

pub struct GithubRunLogsTool;

impl crate::Tool for GithubRunLogsTool {
    fn schema(&self) -> orbit_types::tool::ToolSchema {
        super::gh_schema(
            "github.run.logs",
            "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence. The source stream is read incrementally with an 8 MiB stdout limit and process timeout. A separate diagnostic_unit retains a unique complete failing runner command up to 256 KiB; display truncation does not imply missing command evidence. Source-limit exhaustion is retryable. When the run-scoped read succeeds with no output at all, the excerpt is recovered from the log API of a job the run itself reported failed, and `source` says so.",
            vec![
                super::tool_param("run", "Numeric workflow-run ID", "string", true),
                super::tool_param(
                    "job",
                    "Numeric job ID to narrow the log to one job",
                    "string",
                    false,
                ),
                super::tool_param(
                    "scope",
                    "\"failed\" (default) for failed-step logs, or \"all\" for the full run log — use \"all\" to evidence the checked-out commit, since the checkout step usually succeeds",
                    "string",
                    false,
                ),
                super::tool_param(
                    "max_bytes",
                    "Maximum display excerpt bytes (default 16384, capped at 262144), plus an omission marker. The separate complete diagnostic_unit is capped at 262144 bytes.",
                    "integer",
                    false,
                ),
                super::tool_param(
                    "repo",
                    "Repository in owner/name format (uses current directory if omitted)",
                    "string",
                    false,
                ),
            ],
        )
    }

    fn execute(&self, _ctx: &crate::ToolContext, input: Value) -> Result<Value, OrbitError> {
        let requests = RunLogRequests::from_input(&input)?;
        let max_bytes =
            super::bounded_limit(&input, "max_bytes", DEFAULT_MAX_BYTES, MAX_MAX_BYTES)? as usize;
        let scope = requests.scope;
        let read = read_run_log(&requests, LogReadBounds::new(max_bytes))?;
        let log = read.log;
        let evidence = log.checkout_evidence;

        Ok(json!({
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
            // run-scoped read returned nothing — the job whose own log stood
            // in for it, so a reader is never left guessing what these bytes
            // describe.
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
        }))
    }
}
