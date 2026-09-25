use super::*;

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

/// The read-only GitHub discovery surface.
///
/// These five tools are the only sanctioned way for a task body to enumerate
/// CI state: `gh` runs here, as a child of whichever process executes the
/// tool, and its output is redacted and bounded on the way back out. A body
/// that shells out to `gh` itself gets none of that.
///
/// Nothing that mutates GitHub is registered: the PR pipeline drives those
/// operations directly from `orbit-engine`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(auth::GithubAuthStatusTool);
    registry.register(pr_list::GithubPrListTool);
    registry.register(run_list::GithubRunListTool);
    registry.register(run_logs::GithubRunLogsTool);
    registry.register(run_view::GithubRunViewTool);
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
