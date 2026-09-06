use std::io::Read;

use orbit_common::OrbitError;
use orbit_exec::{ExecRequest, NoSandbox, run_process_streaming_stdout};
use serde_json::{Value, json};

use crate::{TIMEOUT_LONG_MS, check_exec_result};

/// Default excerpt size. Small enough that a routine failed-step read costs an
/// executing agent a few thousand tokens rather than its whole context.
const DEFAULT_MAX_BYTES: u64 = 16_384;

/// Hard ceiling on one call's excerpt, regardless of what the caller asked for.
/// A single unbounded runner log can run to tens of megabytes; that is more
/// than any agent context can hold, so the tool refuses to return it.
const MAX_MAX_BYTES: u64 = 262_144;

/// Cap on returned checkout-evidence lines. Evidence is a handful of lines per
/// job; a much larger match set means the pattern caught noise, not evidence.
const MAX_EVIDENCE_LINES: usize = 40;

/// Which slice of a run's logs to read.
///
/// `failed` is the working default. `all` exists because the checkout step
/// normally *succeeds*, so the commit a runner actually tested is absent from
/// the failed-step log and only `all` can evidence it.
fn log_scope(input: &Value) -> Result<&'static str, OrbitError> {
    match input.get("scope").and_then(Value::as_str) {
        None | Some("failed") => Ok("--log-failed"),
        Some("all") => Ok("--log"),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "invalid `scope`: \"{other}\"; must be \"failed\" or \"all\""
        ))),
    }
}

pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec![
        "run".to_string(),
        "view".to_string(),
        super::require_numeric_id(input, "run")?,
    ];
    if input.get("job").is_some() {
        args.push("--job".to_string());
        args.push(super::require_numeric_id(input, "job")?);
    }
    super::push_repo_flag(&mut args, input)?;
    args.push(log_scope(input)?.to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_LONG_MS))
}

pub struct GithubRunLogsTool;

impl crate::Tool for GithubRunLogsTool {
    fn schema(&self) -> orbit_types::tool::ToolSchema {
        super::gh_schema(
            "github.run.logs",
            "Read a bounded excerpt of one GitHub Actions run's logs — failed steps by default, or the full log — plus runner checkout evidence. The source stream is drained incrementally; checkout extraction stops after 8 MiB and reports incomplete evidence rather than retaining an unbounded log.",
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
                    "Maximum log bytes to return (default 16384, capped at 262144). The excerpt keeps the head and the tail and marks the omitted gap.",
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
        let request = build_exec_request(&input)?;
        let max_bytes =
            super::bounded_limit(&input, "max_bytes", DEFAULT_MAX_BYTES, MAX_MAX_BYTES)? as usize;
        let (result, log) =
            run_process_streaming_stdout(&request, &NoSandbox, move |mut stdout| {
                let mut collector = super::StreamedLogCollector::new(max_bytes, MAX_EVIDENCE_LINES);
                let mut chunk = [0_u8; 4096];
                loop {
                    let read = stdout.read(&mut chunk).map_err(|error| {
                        OrbitError::Execution(format!("failed reading gh run log: {error}"))
                    })?;
                    if read == 0 {
                        return Ok(collector.finish());
                    }
                    collector.push(&chunk[..read]);
                }
            })?;
        check_exec_result(&result, "gh run view --log")?;
        let evidence = log.checkout_evidence;

        Ok(json!({
            "run_id": input.get("run"),
            "scope": input.get("scope").and_then(Value::as_str).unwrap_or("failed"),
            "log": log.text,
            "truncated": log.truncated,
            "returned_bytes": log.returned_bytes,
            "total_bytes": log.total_bytes,
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
