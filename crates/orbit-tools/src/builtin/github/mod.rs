use std::collections::HashSet;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{ToolRegistry, require_str};

pub fn gh_exec_request(
    args: Vec<String>,
    current_dir: Option<String>,
    timeout_ms: u64,
) -> ExecRequest {
    ExecRequest {
        program: "gh".to_string(),
        args,
        current_dir,
        timeout_ms: Some(timeout_ms),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    }
}

pub(super) fn gh_schema(name: &str, description: &str, parameters: Vec<ToolParam>) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: description.to_string(),
        parameters,
        builtin: true,
    }
}

pub(super) fn tool_param(
    name: &str,
    description: &str,
    param_type: &str,
    required: bool,
) -> ToolParam {
    ToolParam {
        name: name.to_string(),
        description: description.to_string(),
        param_type: param_type.to_string(),
        required,
    }
}

macro_rules! gh_tool {
    (
        $vis:vis struct $name:ident;
        name: $tool_name:expr;
        description: $description:expr;
        parameters: [$($param:expr),* $(,)?];
        request: |$request_ctx:ident, $request_input:ident| $request:block
        response: |$response_ctx:ident, $response_input:ident, $result:ident| $response:block
    ) => {
        $vis struct $name;

        impl crate::Tool for $name {
            fn schema(&self) -> orbit_types::tool::ToolSchema {
                super::gh_schema($tool_name, $description, vec![$($param),*])
            }

            fn execute(
                &self,
                ctx: &crate::ToolContext,
                input: serde_json::Value,
            ) -> Result<serde_json::Value, orbit_common::OrbitError> {
                let req = {
                    let $request_ctx = ctx;
                    let $request_input = &input;
                    $request
                }?;
                let exec_result = orbit_exec::run_process(&req, &orbit_exec::NoSandbox)?;
                let $response_ctx = ctx;
                let $response_input = &input;
                let $result = &exec_result;
                $response
            }
        }
    };
    (
        $vis:vis struct $name:ident;
        name: $tool_name:expr;
        description: $description:expr;
        parameters: [$($param:expr),* $(,)?];
        execute: |$execute_ctx:ident, $execute_input:ident| $execute:block
    ) => {
        $vis struct $name;

        impl crate::Tool for $name {
            fn schema(&self) -> orbit_types::tool::ToolSchema {
                super::gh_schema($tool_name, $description, vec![$($param),*])
            }

            fn execute(
                &self,
                ctx: &crate::ToolContext,
                input: serde_json::Value,
            ) -> Result<serde_json::Value, orbit_common::OrbitError> {
                let $execute_ctx = ctx;
                let $execute_input = input;
                $execute
            }
        }
    };
}

pub(super) use gh_tool;

pub mod auth;
pub mod dependabot_alerts;
mod diagnostic;
pub use diagnostic::strip_ansi_sequences;
pub mod logs;
pub mod pr_checkout;
pub mod pr_checks;
pub mod pr_close;
pub mod pr_list;
pub mod repo;
pub mod run_list;
pub mod run_logs;
pub mod run_view;

/// The read-only GitHub discovery surface.
///
/// These five tools are the only sanctioned way for a task body to enumerate
/// CI state: `gh` runs here, as a child of whichever process executes the
/// tool, and its output is redacted and bounded on the way back out. A body
/// that shells out to `gh` itself gets none of that.
///
/// Nothing that mutates GitHub is registered. `pr_checkout`, `pr_checks`,
/// `pr_close`, and `repo` stay unregistered — the PR pipeline drives those
/// operations directly from `orbit-engine`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(auth::GithubAuthStatusTool);
    registry.register(pr_list::GithubPrListTool);
    registry.register(run_list::GithubRunListTool);
    registry.register(run_logs::GithubRunLogsTool);
    registry.register(run_view::GithubRunViewTool);
}

/// Extract a non-empty `pr` field from the tool input.
/// Accepts a numeric PR number or a GitHub PR URL (extracts the number from the path).
pub(super) fn require_pr(input: &Value) -> Result<String, OrbitError> {
    let pr = require_str(input, "pr")?;
    // Already numeric — use directly.
    if !pr.is_empty() && pr.chars().all(|c| c.is_ascii_digit()) {
        return Ok(pr);
    }
    // Try to extract PR number from a GitHub URL like
    // https://github.com/owner/repo/pull/123
    if pr.contains("github.com/")
        && pr.contains("/pull/")
        && let Some(num) = pr.rsplit('/').next()
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(num.to_string());
    }
    Err(OrbitError::InvalidInput(format!(
        "invalid `pr`: \"{pr}\"; must be a numeric PR number or GitHub PR URL"
    )))
}

/// Extract a required numeric GitHub identifier (a workflow-run or job ID).
///
/// Numeric-only is a hardening rule, not a formatting preference: the value is
/// appended to a `gh` argv, and a leading `-` would otherwise be parsed as a
/// flag.
pub(super) fn require_numeric_id(input: &Value, key: &str) -> Result<String, OrbitError> {
    let raw = require_str(input, key)?;
    if raw.chars().all(|c| c.is_ascii_digit()) {
        return Ok(raw);
    }
    Err(OrbitError::InvalidInput(format!(
        "invalid `{key}`: \"{raw}\"; must be a numeric GitHub identifier"
    )))
}

/// Append `--repo <owner/name>` when the caller supplied one.
pub(super) fn push_repo_flag(args: &mut Vec<String>, input: &Value) -> Result<(), OrbitError> {
    push_optional_flag(args, input, "repo", "--repo")
}

/// Append `<flag> <value>` when `key` is present, rejecting a value that would
/// be read as another `gh` flag.
pub(super) fn push_optional_flag(
    args: &mut Vec<String>,
    input: &Value,
    key: &str,
    flag: &str,
) -> Result<(), OrbitError> {
    let Some(value) = input.get(key).and_then(Value::as_str) else {
        return Ok(());
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    if value.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "invalid `{key}`: \"{value}\"; must not start with `-`"
        )));
    }
    args.push(flag.to_string());
    args.push(value.to_string());
    Ok(())
}

/// The `owner/name` path segment for a `gh api repos/...` endpoint.
///
/// Without an explicit `repo`, `gh` resolves its own `{owner}/{repo}`
/// placeholders from the working directory. With one, every character is
/// checked before it reaches a URL path: an unvalidated value would let a
/// caller append query parameters or traverse to another endpoint.
pub(super) fn repository_path(input: &Value) -> Result<String, OrbitError> {
    let Some(raw) = input.get("repo").and_then(Value::as_str) else {
        return Ok("{owner}/{repo}".to_string());
    };
    let repo = raw.trim();
    let mut parts = repo.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid_part = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    if parts.next().is_some() || !valid_part(owner) || !valid_part(name) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid `repo`: \"{repo}\"; expected owner/name using ASCII letters, digits, '.', '-', or '_'"
        )));
    }
    Ok(repo.to_string())
}

/// Read an optional positive bound, clamped into `[1, max]`.
pub(super) fn bounded_limit(
    input: &Value,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64().ok_or_else(|| {
            OrbitError::InvalidInput(format!("`{key}` must be a positive integer"))
        })?,
        Value::String(text) => text.trim().parse::<u64>().map_err(|error| {
            OrbitError::InvalidInput(format!("`{key}` must be a positive integer: {error}"))
        })?,
        Value::Null => return Ok(default),
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "`{key}` must be a positive integer"
            )));
        }
    };
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "`{key}` must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

/// Parse a `gh --json` payload, naming the command in the failure.
pub fn parse_gh_json(stdout: &str, label: &str) -> Result<Value, OrbitError> {
    serde_json::from_str(stdout)
        .map_err(|error| OrbitError::Execution(format!("failed to parse {label} output: {error}")))
}

/// One log excerpt, already redacted and bounded.
pub struct BoundedLog {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: usize,
    pub returned_bytes: usize,
}

/// Cap a log excerpt at `max_bytes`, keeping the head and the tail.
///
/// A failed-step log carries its signal at both ends — the head names the
/// command and its arguments, the tail carries the assertion or exit status —
/// so a plain prefix truncation loses the part the reader came for. The gap is
/// marked inline, and `truncated` lets a caller ask for more rather than
/// silently reasoning over a partial log.
pub fn bound_log_text(raw: &str, max_bytes: usize) -> BoundedLog {
    let redacted = redact_all(raw);
    let total_bytes = redacted.len();
    if total_bytes <= max_bytes {
        return BoundedLog {
            returned_bytes: total_bytes,
            text: redacted,
            truncated: false,
            total_bytes,
        };
    }

    let head_len = floor_char_boundary(&redacted, max_bytes / 2);
    let tail_start =
        ceil_char_boundary(&redacted, total_bytes.saturating_sub(max_bytes - head_len));
    let omitted = tail_start - head_len;
    let text = format!(
        "{}\n[... {omitted} bytes omitted; raise the byte budget for more ...]\n{}",
        &redacted[..head_len],
        &redacted[tail_start..]
    );
    BoundedLog {
        returned_bytes: head_len + (total_bytes - tail_start),
        text,
        truncated: true,
        total_bytes,
    }
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Prose a runner emits around a checkout. Matched case-insensitively.
const CHECKOUT_MARKERS: &[&str] = &[
    "head is now at",
    "-> fetch_head",
    "git checkout --progress",
    "checking out the ref",
    "checkout ref",
];

const MIN_SHA_LEN: usize = 7;
const MAX_SHA_LEN: usize = 40;
/// The most source bytes checkout extraction will inspect from one log. The
/// rest is still drained so `gh` can exit, but identity is marked incomplete.
pub const MAX_CHECKOUT_LOG_SCAN_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHECKOUT_EVIDENCE_LINE_BYTES: usize = 16 * 1024;

/// The commit a runner actually checked out, read out of the runner's own log.
///
/// A workflow event's reported head SHA is metadata; this is evidence. They
/// disagree whenever a merge-queue or pull-request merge commit is what got
/// tested, so the two travel as separate fields and are never merged into one
/// `sha`.
pub struct CheckoutEvidence {
    /// The distinct commits the log named as checked out, with an
    /// abbreviation collapsed into the full SHA it prefixes. More than one
    /// entry means the log genuinely disagreed with itself.
    pub commits: Vec<String>,
    pub lines: Vec<String>,
    /// False only when identity itself may have been missed: the source byte
    /// cap (`source_truncated`) was hit, or a dropped overlong line could
    /// plausibly have carried checkout identity. A reduced *display* — the
    /// evidence line/commit count cap, or an overlong line unrelated to
    /// checkout — does not clear this; see `display_truncated`.
    pub complete: bool,
    pub scanned_bytes: usize,
    pub source_truncated: bool,
    /// True when what is reported was capped for display reasons that do not
    /// bear on checkout identity: the evidence line or commit count limit was
    /// reached, or an overlong line unrelated to checkout was dropped.
    pub display_truncated: bool,
}

/// A bounded excerpt and checkout evidence collected while a log is drained.
/// No unbounded copy of the source log is retained.
pub struct StreamedLog {
    pub text: String,
    pub truncated: bool,
    pub total_bytes: usize,
    pub returned_bytes: usize,
    pub checkout_evidence: CheckoutEvidence,
    /// Complete runner command evidence, independent of display truncation.
    pub diagnostic: Option<String>,
    /// Selected failure regions when the complete command exceeds its bound.
    pub failure_regions: Option<Value>,
    /// False when a source limit, invalid line, or explicit truncation notice
    /// prevents complete diagnostic collection; independent of display size.
    pub source_complete: bool,
}

/// Incrementally retain the head/tail excerpt and checkout evidence from a
/// potentially large log. `scan_limit` makes incomplete identity explicit
/// instead of retaining an unbounded source stream.
pub struct StreamedLogCollector {
    max_bytes: usize,
    /// How much of the budget the head window may keep; the rest is the tail.
    head_bytes: usize,
    head: Vec<u8>,
    tail: Vec<u8>,
    total_bytes: usize,
    evidence: CheckoutEvidenceCollector,
    diagnostic: diagnostic::DiagnosticCollector,
}

impl StreamedLogCollector {
    /// An even head/tail split, for a source whose every line is relevant —
    /// `gh run view --log-failed` emits only failed steps, so its head names
    /// the failing command and its tail carries the assertion.
    pub fn new(max_bytes: usize, max_evidence_lines: usize) -> Self {
        Self::with_head_bytes(max_bytes, max_bytes / 2, max_evidence_lines)
    }

    /// A tail-weighted split, for a whole-job log.
    ///
    /// Such a log opens with runner provisioning and setup and only reaches
    /// the failing step near its end, so an even split spends half the budget
    /// on boilerplate and can cut the diagnostic headline off. A quarter is
    /// still enough head to name the job and its first command.
    pub fn tail_weighted(max_bytes: usize, max_evidence_lines: usize) -> Self {
        Self::with_head_bytes(max_bytes, max_bytes / 4, max_evidence_lines)
    }

    fn with_head_bytes(max_bytes: usize, head_bytes: usize, max_evidence_lines: usize) -> Self {
        Self {
            max_bytes,
            head_bytes,
            head: Vec::with_capacity(head_bytes),
            tail: Vec::with_capacity(max_bytes.saturating_sub(head_bytes)),
            total_bytes: 0,
            diagnostic: diagnostic::DiagnosticCollector::default(),
            evidence: CheckoutEvidenceCollector::new(
                max_evidence_lines,
                MAX_CHECKOUT_LOG_SCAN_BYTES,
            ),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        self.evidence.push(chunk);
        self.diagnostic.push(chunk);

        let head_limit = self.head_bytes;
        let head_take = head_limit.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..head_take]);
        let tail_limit = self.max_bytes.saturating_sub(head_limit);
        self.tail.extend_from_slice(&chunk[head_take..]);
        if self.tail.len() > tail_limit {
            self.tail.drain(..self.tail.len() - tail_limit);
        }
    }

    pub fn finish(mut self) -> StreamedLog {
        let evidence = self.evidence.finish();
        let truncated = self.total_bytes > self.max_bytes;
        let text = if truncated {
            let omitted = self
                .total_bytes
                .saturating_sub(self.head.len() + self.tail.len());
            format!(
                "{}\n[... {omitted} bytes omitted; raise the byte budget for more ...]\n{}",
                redact_all(&String::from_utf8_lossy(&self.head)),
                redact_all(&String::from_utf8_lossy(&self.tail)),
            )
        } else {
            // When the source is short, all bytes are in `tail` except the
            // initial head window, so join the two retained halves.
            self.head.extend_from_slice(&self.tail);
            redact_all(&String::from_utf8_lossy(&self.head))
        };
        let source_complete = self.diagnostic.source_complete();
        let (diagnostic, failure_regions) = self.diagnostic.finish();
        StreamedLog {
            returned_bytes: text.len(),
            text,
            truncated,
            total_bytes: self.total_bytes,
            checkout_evidence: evidence,
            source_complete,
            diagnostic,
            failure_regions,
        }
    }
}

struct CheckoutEvidenceCollector {
    max_lines: usize,
    scan_limit: usize,
    scanned_bytes: usize,
    source_truncated: bool,
    pending_line: String,
    dropping_line: bool,
    commits: Vec<String>,
    seen_commits: HashSet<String>,
    /// Of the observed tokens, the ones a line actually named as the commit
    /// that was checked out — as opposed to a merge parent or a fetched ref
    /// that happened to share the line.
    named_commits: HashSet<String>,
    /// A recognized command whose immediately following output line may name
    /// the checkout. Keep only one bounded line: an unrelated intervening
    /// line consumes it instead of letting a later SHA inherit its authority.
    pending_checkout_command: Option<String>,
    lines: Vec<String>,
    complete: bool,
    display_truncated: bool,
}

impl CheckoutEvidenceCollector {
    fn new(max_lines: usize, scan_limit: usize) -> Self {
        Self {
            max_lines,
            scan_limit,
            scanned_bytes: 0,
            source_truncated: false,
            pending_line: String::new(),
            dropping_line: false,
            commits: Vec::new(),
            seen_commits: HashSet::new(),
            named_commits: HashSet::new(),
            pending_checkout_command: None,
            lines: Vec::new(),
            complete: true,
            display_truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        let remaining = self.scan_limit.saturating_sub(self.scanned_bytes);
        let scanned = &chunk[..chunk.len().min(remaining)];
        self.scanned_bytes += scanned.len();
        if scanned.len() < chunk.len() {
            self.source_truncated = true;
            self.complete = false;
        }
        for part in String::from_utf8_lossy(scanned).split_inclusive('\n') {
            if self.dropping_line {
                if part.ends_with('\n') {
                    self.dropping_line = false;
                }
                continue;
            }
            if self.pending_line.len() + part.len() > MAX_CHECKOUT_EVIDENCE_LINE_BYTES {
                let identity_bearing = overlong_line_is_identity_bearing(&self.pending_line, part);
                self.pending_line.clear();
                self.dropping_line = !part.ends_with('\n');
                // A dropped physical line still consumes a preceding
                // `git log` command. Otherwise a later unrelated SHA could
                // inherit authority that belonged only to this line.
                self.pending_checkout_command = None;
                if identity_bearing {
                    self.complete = false;
                } else {
                    self.display_truncated = true;
                }
                continue;
            }
            self.pending_line.push_str(part);
            if self.pending_line.ends_with('\n') {
                self.observe_pending_line();
            }
        }
    }

    fn finish(mut self) -> CheckoutEvidence {
        if !self.pending_line.is_empty() && !self.dropping_line {
            self.observe_pending_line();
        }
        CheckoutEvidence {
            commits: canonical_checkout_commits(&self.commits, &self.named_commits),
            lines: self.lines,
            complete: self.complete,
            scanned_bytes: self.scanned_bytes,
            source_truncated: self.source_truncated,
            display_truncated: self.display_truncated,
        }
    }

    fn observe_pending_line(&mut self) {
        let line = std::mem::take(&mut self.pending_line);
        let (step, payload) = split_log_line(&line);
        let lowered = payload.to_ascii_lowercase();
        let marked = CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker));
        let in_checkout_step = step.to_ascii_lowercase().contains("checkout");
        let command_output = self
            .pending_checkout_command
            .take()
            .filter(|_| is_bare_commit_sha(payload));
        let command = checkout_identity_command(payload);
        if !(marked || in_checkout_step && is_bare_commit_sha(payload) || command_output.is_some())
        {
            self.pending_checkout_command = command;
            return;
        }
        if let Some(command) = &command_output {
            self.record_evidence_line(command);
        }
        self.record_evidence_line(payload);
        let named = named_checkout_commit(
            payload,
            &lowered,
            in_checkout_step || command_output.is_some(),
        );
        for token in commit_sha_tokens(payload) {
            if named == Some(token) {
                self.named_commits.insert(token.to_string());
            }
            if self.seen_commits.contains(token) {
                continue;
            }
            if self.commits.len() == self.max_lines {
                self.display_truncated = true;
                continue;
            }
            let token = token.to_string();
            self.seen_commits.insert(token.clone());
            self.commits.push(token);
        }
        self.pending_checkout_command = command;
    }

    fn record_evidence_line(&mut self, payload: &str) {
        if self.lines.len() < self.max_lines {
            self.lines.push(redact_all(payload.trim()));
        } else {
            self.display_truncated = true;
        }
    }
}

/// Split one `gh run view --log` line into its step column and its payload.
///
/// The line format is `job<TAB>step<TAB>timestamp payload`. The split matters:
/// the *step* column names a pinned action (`actions/checkout@<sha>`), and
/// harvesting that SHA would report the checkout action's own commit as the
/// commit under test — the exact conflation this scan exists to prevent.
fn split_log_line(line: &str) -> (&str, &str) {
    let line = line.trim_start_matches('\u{feff}');
    let mut columns = line.splitn(3, '\t');
    let (step, rest) = match (columns.next(), columns.next(), columns.next()) {
        (Some(_job), Some(step), Some(rest)) => (step, rest),
        _ => ("", line),
    };
    // Drop the leading ISO-8601 runner timestamp when there is one.
    let payload = match rest.split_once(' ') {
        Some((first, tail)) if first.contains('T') && first.ends_with('Z') => tail,
        _ => rest,
    };
    (step, payload)
}

/// Whether a line dropped for exceeding [`MAX_CHECKOUT_EVIDENCE_LINE_BYTES`]
/// could plausibly have carried checkout identity, judged from whatever
/// prefix was captured before the drop.
///
/// The job/step columns and runner timestamp sit at the very start of a
/// `gh run view --log` line, long before any payload could reach this
/// threshold, so the accumulated prefix (or, when nothing had accumulated yet,
/// a short probe from the start of the new part) is enough to classify it
/// without buffering the whole overlong line.
fn overlong_line_is_identity_bearing(pending: &str, part: &str) -> bool {
    let probe = if pending.is_empty() {
        let cap = ceil_char_boundary(part, part.len().min(512));
        &part[..cap]
    } else {
        pending
    };
    let (step, payload) = split_log_line(probe);
    let lowered = payload.to_ascii_lowercase();
    step.to_ascii_lowercase().contains("checkout")
        || CHECKOUT_MARKERS
            .iter()
            .any(|marker| lowered.contains(marker))
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

/// Whether a payload is a bare commit SHA on its own line.
///
/// `actions/checkout` prints the resolved commit this way, with no surrounding
/// prose, which is often the only place a branch-mode checkout records what it
/// actually landed on.
fn is_bare_commit_sha(payload: &str) -> bool {
    let trimmed = payload.trim();
    trimmed.len() == MAX_SHA_LEN && trimmed.bytes().all(is_hex)
}

/// Recognize the runner command that prints the checked-out `HEAD` as its
/// next line. This intentionally accepts only `git log -1 --format=%H`, not
/// a generic bare SHA after any command: ordinary test output and arbitrary
/// repository probes must not become checkout identity.
fn checkout_identity_command(payload: &str) -> Option<String> {
    let command = payload.strip_prefix("[command]")?.trim();
    let mut words = command.split_ascii_whitespace();
    let program = words.next()?;
    if program.rsplit('/').next()? != "git"
        || !matches!(words.next(), Some("log"))
        || !matches!(words.next(), Some("-1"))
        || !matches!(words.next(), Some("--format=%H"))
        || words.next().is_some()
    {
        return None;
    }
    Some(command.to_string())
}

/// The prose `actions/checkout` prints in front of the commit it landed on.
const HEAD_IS_NOW_AT: &str = "head is now at";

/// The commit this line *names* as checked out, if it names one.
///
/// `HEAD is now at 7dcd45b Merge 4968f13 into abc1234` reports one checked-out
/// commit and then quotes the merge's two parents inside the commit subject.
/// Only the token directly after the marker is identity; the rest is prose
/// that happens to be hex. Everything else — a fetched ref, a merge summary —
/// names no commit and stays incidental evidence.
fn named_checkout_commit<'a>(
    payload: &'a str,
    lowered: &str,
    in_checkout_step: bool,
) -> Option<&'a str> {
    if let Some(marker) = lowered.find(HEAD_IS_NOW_AT) {
        // `to_ascii_lowercase` rewrites ASCII bytes in place and leaves every
        // multi-byte sequence alone, so offsets into `lowered` index `payload`.
        return commit_sha_tokens(&payload[marker + HEAD_IS_NOW_AT.len()..])
            .into_iter()
            .next();
    }
    (in_checkout_step && is_bare_commit_sha(payload)).then(|| payload.trim())
}

/// Reduce the observed SHA tokens to the distinct commits actually checked out.
///
/// Two reductions, neither of which relaxes identity. A line that *names* the
/// checked-out commit outranks one that merely quotes a merge parent or a
/// fetched ref. An abbreviation and the full SHA it prefixes are one commit,
/// not two — the same runner routinely prints both forms. A real disagreement
/// between two checkout steps survives both reductions and is still reported
/// as two commits, and an abbreviation with more than one distinct expansion
/// is left abbreviated rather than resolved to a guess.
fn canonical_checkout_commits(observed: &[String], named: &HashSet<String>) -> Vec<String> {
    let identity: Vec<&String> = if observed.iter().any(|commit| named.contains(commit)) {
        observed
            .iter()
            .filter(|commit| named.contains(*commit))
            .collect()
    } else {
        observed.iter().collect()
    };

    let mut canonical: Vec<String> = Vec::new();
    for commit in identity {
        // `observed` holds distinct tokens, so two expansions of the same
        // length are two different commits and the abbreviation cannot be
        // resolved to either of them.
        let mut longest: Option<&String> = None;
        let mut contested = false;
        for candidate in observed
            .iter()
            .filter(|candidate| candidate.len() > commit.len() && candidate.starts_with(&**commit))
        {
            match longest {
                Some(current) if current.len() > candidate.len() => {}
                Some(current) if current.len() == candidate.len() => contested = true,
                _ => {
                    longest = Some(candidate);
                    contested = false;
                }
            }
        }
        let resolved = if contested {
            commit
        } else {
            longest.unwrap_or(commit)
        };
        if !canonical.contains(resolved) {
            canonical.push(resolved.clone());
        }
    }
    canonical
}

/// Collect every lowercase-hex token of commit-SHA length in `payload`.
///
/// A token preceded by `@` is skipped: that is the `owner/action@sha` pin
/// form, which identifies the action, not the commit under test.
fn commit_sha_tokens(payload: &str) -> Vec<&str> {
    let bytes = payload.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if !bytes[index].is_ascii_alphanumeric() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
            index += 1;
        }
        let token = &payload[start..index];
        let pinned = start > 0 && bytes[start - 1] == b'@';
        if !pinned
            && (MIN_SHA_LEN..=MAX_SHA_LEN).contains(&token.len())
            && token.bytes().all(is_hex)
        {
            tokens.push(token);
        }
    }
    tokens
}

/// Scan a runner log for checkout evidence, keeping at most `max_lines` lines
/// and at most `max_lines` distinct commits.
///
/// The scan deliberately covers the whole raw log, not the bounded excerpt,
/// so both outputs must be capped here: a matrix run whose log carries tens
/// of thousands of fetch lines must not hand the caller an unbounded commit
/// list, and membership is a set lookup rather than a scan per token.
pub fn scan_checkout_evidence(log: &str, max_lines: usize) -> CheckoutEvidence {
    let mut collector = CheckoutEvidenceCollector::new(max_lines, log.len());
    collector.push(log.as_bytes());
    collector.finish()
}

pub(crate) mod landing;
#[cfg(test)]
mod tests;
