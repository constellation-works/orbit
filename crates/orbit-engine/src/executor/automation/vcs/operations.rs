use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::jitter::JitterRng;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::workflow::ForgeUnavailableHold;
use serde_json::{Value, json};

pub(crate) const PUSH: &str = "push";
/// [ORB-14338] Push one commit to a run-owned `refs/orbit/candidates/` ref.
pub(crate) const CANDIDATE_REF_PUSH: &str = "push.candidate_ref";
/// The namespace a carried candidate ref lives under on `origin`.
pub(crate) const CANDIDATE_REF_PREFIX: &str = "refs/orbit/candidates/";
pub(crate) const PR_LIST: &str = "pr.list";
pub(crate) const PR_CREATE: &str = "pr.create";
pub(crate) const PR_VIEW: &str = "pr.view";
pub(crate) const PR_MERGE: &str = "pr.merge";
pub(crate) const PR_MERGE_CAPABILITIES: &str = "pr.merge_capabilities";
pub(crate) const PR_STATUS: &str = "pr.status";

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const SLOW_TIMEOUT_MS: u64 = 30_000;
const LONG_TIMEOUT_MS: u64 = 60_000;

/// Bounded attempts for a private automation PR lookup, including the first
/// try. `pr_open`'s existing-PR check must survive a single GitHub API blip
/// rather than abort a delivery whose branch is already pushed (F2026-09-011).
const GITHUB_LOOKUP_TRANSIENT_ATTEMPTS: u32 = 3;
const GITHUB_LOOKUP_TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(500);

/// A forge incident refuses pushes for minutes, not milliseconds
/// [ORB-14617]: six attempts with exponential backoff of 10 s doubling to a
/// 160 s cap wait at most 310 s in all. Each wait draws equal jitter (half
/// fixed, half random), so pushes failing together do not retry in lockstep.
/// A cancel stops the worker's process group, which ends a wait with it.
const PUSH_TRANSIENT_ATTEMPTS: u32 = 6;
const PUSH_TRANSIENT_INITIAL_BACKOFF_MS: u64 = 10_000;
const PUSH_TRANSIENT_BACKOFF_CAP_MS: u64 = 160_000;
/// Bounds on a `forge_retry` override, so asset input cannot turn the push
/// into an unbounded wait.
const PUSH_TRANSIENT_MAX_ATTEMPTS: u64 = 10;
const PUSH_TRANSIENT_MAX_BACKOFF_MS: u64 = 600_000;
/// The longest `forge_retry.window_ms` [ORB-14634]: the two hours the clock
/// keeps resuming a held owner-local run.
const PUSH_FORGE_MAX_WINDOW_MS: u64 = 2 * 60 * 60 * 1000;
/// Largest forge refusal a hold carries.
const FORGE_HOLD_DIAGNOSTIC_BYTES: usize = 2048;

/// GitHub's reason for refusing a merge because another merge moved the base
/// between its mergeability check and the mutation (F2026-10-071). The `sha`
/// condition was not what failed.
const BASE_MODIFIED_REASON: &str = "Base branch was modified. Review and try the merge again.";
/// The `refusal` a reviewed merge reports, instead of failing, when its
/// caller opts in with `report_base_modified`.
pub(crate) const BASE_MODIFIED_REFUSAL: &str = "base_modified";

/// Execute the VCS operations owned by deterministic shipment automation.
///
/// This boundary is deliberately separate from `ToolRegistry`: the operation
/// labels are engine-private, are never advertised to agents, and do not pass
/// through public tool authorization or activity allowlists.
pub(crate) fn run(operation: &str, input: &Value) -> Result<Value, OrbitError> {
    match operation {
        PUSH => push(input),
        CANDIDATE_REF_PUSH => push_candidate_ref(input),
        PR_LIST => pr_list(input),
        PR_CREATE => pr_create(input),
        PR_VIEW => pr_view(input),
        PR_MERGE => pr_merge(input),
        PR_MERGE_CAPABILITIES => pr_merge_capabilities(input),
        PR_STATUS => pr_status(input),
        other => Err(OrbitError::InvalidInput(format!(
            "unknown private automation VCS operation '{other}'"
        ))),
    }
}

fn push(input: &Value) -> Result<Value, OrbitError> {
    let repo_root = required_string(input, "repo_root")?;
    let branch = required_string(input, "branch")?;
    let force_with_lease = input
        .get("force_with_lease")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expected_remote_sha = optional_string(input, "expected_remote_sha");

    reject_option_like("branch", branch)?;
    if force_with_lease && !valid_expected_remote_sha(expected_remote_sha) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS push requires an exact 40- or 64-character expected_remote_sha when force_with_lease is true"
                .to_string(),
        ));
    }

    let mut args = vec!["push".to_string()];
    if let Some(expected_remote_sha) = force_with_lease.then_some(expected_remote_sha).flatten() {
        args.push(format!(
            "--force-with-lease=refs/heads/{branch}:{expected_remote_sha}"
        ));
    }
    args.extend(["--".to_string(), "origin".to_string(), branch.to_string()]);
    let target_ref = format!("refs/heads/{branch}");
    let local_head = execute(
        "git",
        vec!["rev-parse".into(), "--verify".into(), target_ref.clone()],
        Some(Path::new(repo_root)),
        DEFAULT_TIMEOUT_MS,
        "push local head",
    )?;
    let pushed = execute_push_with_transient_retry(
        &args,
        Path::new(repo_root),
        &target_ref,
        local_head.stdout.trim(),
        "push",
        PushBackoff::from_input(input)?,
    )
    .map_err(|exhausted| exhausted.into_hold_error())?;
    Ok(pushed.output())
}

/// Push `head_sha` to `target_ref` on `origin`, replacing whatever the ref
/// held: the ref is owned by the one run named in it.
fn push_candidate_ref(input: &Value) -> Result<Value, OrbitError> {
    let repo_root = required_string(input, "repo_root")?;
    let head_sha = required_string(input, "head_sha")?;
    let target_ref = required_string(input, "target_ref")?;
    if !valid_expected_remote_sha(Some(head_sha)) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS candidate push requires an exact 40- or 64-character head_sha"
                .to_string(),
        ));
    }
    if !valid_candidate_ref(target_ref) {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS candidate push target must be a plain ref under \
             '{CANDIDATE_REF_PREFIX}'"
        )));
    }
    // A carried candidate ref is pushed by a failure activity, which has no
    // later step to hold at: exhausting its budget is an ordinary failure.
    let pushed = execute_push_with_transient_retry(
        &[
            "push".to_string(),
            "--".to_string(),
            "origin".to_string(),
            format!("+{head_sha}:{target_ref}"),
        ],
        Path::new(repo_root),
        target_ref,
        head_sha,
        "candidate ref push",
        PushBackoff::from_input(input)?,
    )
    .map_err(PushFailure::into_error)?;
    Ok(pushed.output())
}

/// The transient push retry budget: `forge_retry` in the operation input, or
/// the production default.
///
/// [ORB-14634] `window_ms` keeps a push retrying past `attempts`, at the
/// capped backoff, until that long after the forge first refused it. A
/// claimed leaf sets it: its run cannot be resumed the way the clock resumes
/// a held owner-local run, so it keeps its claim and retries in place.
#[derive(Debug, Clone, Copy)]
struct PushBackoff {
    attempts: u32,
    initial_ms: u64,
    cap_ms: u64,
    window_ms: Option<u64>,
}

impl PushBackoff {
    fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let Some(retry) = input.get("forge_retry").filter(|value| !value.is_null()) else {
            return Ok(Self {
                attempts: PUSH_TRANSIENT_ATTEMPTS,
                initial_ms: PUSH_TRANSIENT_INITIAL_BACKOFF_MS,
                cap_ms: PUSH_TRANSIENT_BACKOFF_CAP_MS,
                window_ms: None,
            });
        };
        let bounded = |key: &str, min: u64, max: u64| {
            retry
                .get(key)
                .and_then(Value::as_u64)
                .filter(|value| (min..=max).contains(value))
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "private automation VCS push forge_retry.{key} must be an integer from \
                         {min} to {max}"
                    ))
                })
        };
        let attempts = bounded("max_attempts", 1, PUSH_TRANSIENT_MAX_ATTEMPTS)?;
        let initial_ms = bounded("initial_backoff_ms", 0, PUSH_TRANSIENT_MAX_BACKOFF_MS)?;
        let cap_ms = bounded("backoff_cap_ms", initial_ms, PUSH_TRANSIENT_MAX_BACKOFF_MS)?;
        let window_ms = match retry.get("window_ms") {
            None | Some(Value::Null) => None,
            Some(_) => Some(bounded("window_ms", 0, PUSH_FORGE_MAX_WINDOW_MS)?),
        };
        Ok(Self {
            attempts: attempts as u32,
            initial_ms,
            cap_ms,
            window_ms,
        })
    }

    /// Whether a push refused for the `attempt`th time, `refused_for` after
    /// its first refusal, stops retrying.
    fn spent(self, attempt: u32, refused_for: Duration) -> bool {
        attempt >= self.attempts
            && self
                .window_ms
                .is_none_or(|window| refused_for >= Duration::from_millis(window))
    }

    /// The wait before attempt `attempt + 1`: exponential, capped, with
    /// equal jitter.
    fn delay_ms(self, attempt: u32, jitter: &mut JitterRng) -> u64 {
        let ceiling = self
            .initial_ms
            .saturating_mul(1_u64 << (attempt - 1).min(32))
            .min(self.cap_ms);
        let fixed = ceiling / 2;
        fixed + jitter.full_jitter(ceiling - fixed)
    }
}

/// A push that reached the remote, with what its retries cost.
struct Pushed {
    result: orbit_exec::ExecutionResult,
    attempts: u32,
    waited_ms: u64,
}

impl Pushed {
    fn output(&self) -> Value {
        json!({
            "stdout": self.result.stdout,
            "stderr": self.result.stderr,
            "attempts": self.attempts,
            "waited_ms": self.waited_ms,
        })
    }
}

/// Why a push did not reach the remote.
enum PushFailure {
    /// Not retried: a permanent refusal, a timeout, a failed confirmation.
    Error(OrbitError),
    /// Every attempt in the budget was refused for a transient reason.
    Exhausted(Box<PushExhausted>),
}

struct PushExhausted {
    target_ref: String,
    head_sha: String,
    attempts: u32,
    waited_ms: u64,
    /// When the forge first refused this push.
    first_refused_at: DateTime<Utc>,
    diagnostic: String,
    error: OrbitError,
}

impl PushFailure {
    fn into_error(self) -> OrbitError {
        match self {
            Self::Error(error) => error,
            Self::Exhausted(exhausted) => exhausted.error,
        }
    }

    /// An exhausted budget carries a typed [`ForgeUnavailableHold`]: the
    /// forge, not the candidate, refused the push.
    fn into_hold_error(self) -> OrbitError {
        let exhausted = match self {
            Self::Error(error) => return error,
            Self::Exhausted(exhausted) => *exhausted,
        };
        let hold = ForgeUnavailableHold {
            target_ref: exhausted.target_ref,
            head_sha: exhausted.head_sha,
            attempts: exhausted.attempts,
            waited_ms: exhausted.waited_ms,
            diagnostic: bounded_diagnostic(&exhausted.diagnostic),
            step_id: String::new(),
            held_at: Utc::now(),
            held_since: exhausted.first_refused_at,
        };
        OrbitError::Execution(hold.text(&format!(
            "the forge refused the push {} times over {} s: {}",
            exhausted.attempts,
            exhausted.waited_ms / 1000,
            exhausted.error
        )))
    }
}

fn bounded_diagnostic(text: &str) -> String {
    let text = text.trim();
    if text.len() <= FORGE_HOLD_DIAGNOSTIC_BYTES {
        return text.to_string();
    }
    let mut end = FORGE_HOLD_DIAGNOSTIC_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Retry only transient push failures, confirming the exact remote ref before
/// resending. A lost reply may hide a successful push, including one that
/// consumed its force-with-lease. A failed remote read stops further mutations.
fn execute_push_with_transient_retry(
    args: &[String],
    repo_root: &Path,
    target_ref: &str,
    head_sha: &str,
    operation: &str,
    backoff: PushBackoff,
) -> Result<Pushed, PushFailure> {
    if !valid_expected_remote_sha(Some(head_sha)) {
        return Err(PushFailure::Error(OrbitError::Execution(
            "private automation VCS push could not resolve the local head SHA".into(),
        )));
    }
    let mut jitter = JitterRng::from_entropy();
    let mut waited_ms = 0_u64;
    let mut attempt = 1;
    let mut first_refusal = None;
    loop {
        let result = run_vcs_process("git", args.to_vec(), Some(repo_root), LONG_TIMEOUT_MS)
            .map_err(PushFailure::Error)?;
        let diagnostic = format!("{}\n{}", result.stdout, result.stderr);
        if result.success || result.timed_out || !is_transient_push_failure(&diagnostic) {
            return succeeded(result, operation)
                .map(|result| Pushed {
                    result,
                    attempts: attempt,
                    waited_ms,
                })
                .map_err(PushFailure::Error);
        }

        let remote = execute(
            "git",
            vec![
                "ls-remote".into(),
                "--refs".into(),
                "--".into(),
                "origin".into(),
                target_ref.into(),
            ],
            Some(repo_root),
            LONG_TIMEOUT_MS,
            "push remote confirmation",
        )
        .map_err(PushFailure::Error)?;
        if remote.stdout.lines().any(|line| {
            let mut fields = line.split_whitespace();
            fields
                .next()
                .is_some_and(|sha| sha.eq_ignore_ascii_case(head_sha))
                && fields.next() == Some(target_ref)
                && fields.next().is_none()
        }) {
            tracing::info!(
                operation,
                attempt,
                waited_ms,
                "confirmed push landed after a transient failure"
            );
            return Ok(Pushed {
                result: remote,
                attempts: attempt,
                waited_ms,
            });
        }
        let (first_refused, first_refused_at) =
            *first_refusal.get_or_insert_with(|| (Instant::now(), Utc::now()));
        if backoff.spent(attempt, first_refused.elapsed()) {
            tracing::warn!(
                operation,
                attempts = attempt,
                waited_ms,
                target_ref,
                head_sha,
                "push retry budget exhausted by transient remote failures"
            );
            let error = succeeded(result, operation).err().unwrap_or_else(|| {
                OrbitError::Execution(format!("private automation VCS {operation} failed"))
            });
            return Err(PushFailure::Exhausted(Box::new(PushExhausted {
                target_ref: target_ref.to_string(),
                head_sha: head_sha.to_string(),
                attempts: attempt,
                waited_ms,
                first_refused_at,
                diagnostic,
                error,
            })));
        }
        let delay_ms = backoff.delay_ms(attempt, &mut jitter);
        tracing::warn!(
            operation,
            attempt,
            delay_ms,
            waited_ms,
            target_ref,
            head_sha,
            past_budget = attempt >= backoff.attempts,
            "retrying push after a transient remote failure"
        );
        std::thread::sleep(Duration::from_millis(delay_ms));
        waited_ms = waited_ms.saturating_add(delay_ms);
        attempt += 1;
    }
}

/// Permanent diagnostics take precedence over transport errors: a server may
/// print both a policy refusal and a generic RPC or remote-rejection message.
fn is_transient_push_failure(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    if [
        "authentication",
        "permission",
        "access denied",
        "not granted",
        "not allowed",
        "not permitted",
        "access rights",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "bad credentials",
        "repository not found",
        "http 401",
        "http 403",
        "error: 401",
        "error: 403",
        "forbidden",
        "non-fast-forward",
        "fetch first",
        "stale info",
        "force-with-lease",
        "hook",
        "protect",
        "declined",
        "gh006",
        "gh013",
        "rule violation",
    ]
    .iter()
    .any(|permanent| text.contains(permanent))
    {
        return false;
    }
    // GitHub names a server-side fault in the rejection's reason, as in
    // `! [remote rejected] head -> head (Internal Server Error)` [ORB-14617].
    text.lines().any(|line| {
        let line = line.trim_end();
        line.contains("[remote rejected]")
            && [
                "(failed)",
                "(internal server error)",
                "(service unavailable)",
                "(bad gateway)",
                "(gateway timeout)",
            ]
            .iter()
            .any(|reason| line.ends_with(reason))
    }) || text
        .lines()
        .any(|line| line.trim() == "remote: internal server error")
        || ["http ", "returned error: "].iter().any(|prefix| {
            text.match_indices(prefix).any(|(index, _)| {
                let status = &text[index + prefix.len()..];
                let bytes = status.as_bytes();
                bytes.len() >= 3
                    && bytes[0] == b'5'
                    && bytes[1].is_ascii_digit()
                    && bytes[2].is_ascii_digit()
                    && bytes.get(3).is_none_or(|byte| !byte.is_ascii_digit())
            })
        })
        || text.contains("rpc failed")
        || text.contains("early eof")
        || text.contains("connection reset")
        || (text.contains("tls handshake")
            && (text.contains("timeout") || text.contains("timed out")))
}

/// Whether `value` is a ref under [`CANDIDATE_REF_PREFIX`] made of plain
/// path segments, so it can be neither an option nor a refspec.
pub(crate) fn valid_candidate_ref(value: &str) -> bool {
    value
        .strip_prefix(CANDIDATE_REF_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && rest.split('/').all(|segment| {
                    !segment.is_empty()
                        && !segment.starts_with('.')
                        && !segment.ends_with(".lock")
                        && segment.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                        })
                        && !segment.contains("..")
                })
        })
}

fn pr_list(input: &Value) -> Result<Value, OrbitError> {
    let head = required_string(input, "head")?;
    let state = optional_string(input, "state").unwrap_or("open");
    let workspace_path = required_string(input, "workspace_path")?;
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        state.to_string(),
        "--head".to_string(),
        head.to_string(),
        "--json".to_string(),
        "number,title,headRefName,author".to_string(),
    ];
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR list",
    )?;
    let pull_requests: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR list returned invalid JSON: {error}"
        ))
    })?;
    if !pull_requests.is_array() {
        return Err(OrbitError::Execution(
            "private automation VCS PR list did not return an array".to_string(),
        ));
    }
    Ok(json!({ "pull_requests": pull_requests }))
}

fn pr_create(input: &Value) -> Result<Value, OrbitError> {
    let title = required_string(input, "title")?;
    let body = required_string(input, "body")?;
    let base = required_string(input, "base")?;
    let head = required_string(input, "head")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
        "--base".to_string(),
        base.to_string(),
        "--head".to_string(),
        head.to_string(),
    ];
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR create",
    )?;
    Ok(json!({
        "url": result.stdout.trim(),
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

fn pr_view(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !valid_pr_selector(selector) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        )));
    }
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        selector.to_string(),
        "--json".to_string(),
        "number,title,body,headRefName,files,commits,url".to_string(),
    ];
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR view",
    )?;
    let pull_request: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR view returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": pull_request }))
}

fn pr_merge(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let strategy = optional_string(input, "strategy").unwrap_or("squash");
    let strategy_flag = match strategy {
        "squash" => "--squash",
        "merge" => "--merge",
        "rebase" => "--rebase",
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "invalid private automation VCS merge strategy '{other}'"
            )));
        }
    };
    // `--auto` queues GitHub's own auto-merge, which still waits for every
    // required check and branch protection. There is deliberately no
    // administrative bypass (`--admin`) in this surface.
    let auto = input.get("auto").and_then(Value::as_bool).unwrap_or(false);
    if let Some(reviewed_head) = optional_string(input, "reviewed_head_sha") {
        if auto {
            return Err(OrbitError::InvalidInput(
                "review_gate_stale: deferred auto-merge cannot guarantee the reviewed head; wait for checks and request a synchronous merge".to_string(),
            ));
        }
        let report_base_modified = input
            .get("report_base_modified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return pr_merge_reviewed(
            selector,
            workspace_path,
            strategy,
            reviewed_head,
            report_base_modified,
        );
    }
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        selector.to_string(),
        strategy_flag.to_string(),
    ];
    if auto {
        args.push("--auto".to_string());
    }
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR merge",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

/// Use the synchronous REST mutation: `sha` is checked by GitHub when it
/// merges, and this endpoint never enables auto-merge or enters a merge queue.
/// `gh pr merge --match-head-commit` alone is insufficient because the CLI
/// can choose deferred semantics for a queue-required branch.
///
/// [ORB-14205] With `report_base_modified`, the provider's base-modification
/// refusal returns `{"merged": false, "refusal": "base_modified"}` with the
/// raw output, so the caller can decide whether to ask again. Every other
/// failure, and that one without the opt-in, stays an error.
fn pr_merge_reviewed(
    selector: &str,
    workspace_path: &str,
    strategy: &str,
    reviewed_head: &str,
    report_base_modified: bool,
) -> Result<Value, OrbitError> {
    // Managed completion supplies a number in the current repository. Refuse
    // other selectors here rather than resolving a URL into a different repo.
    if selector.is_empty() || !selector.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires a pull request number in the workspace repository".into(),
        ));
    }
    if !valid_expected_remote_sha(Some(reviewed_head)) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires an exact 40- or 64-character reviewed_head_sha".into(),
        ));
    }
    let result = run_vcs_process(
        "gh",
        vec![
            "api".to_string(),
            format!("repos/{{owner}}/{{repo}}/pulls/{selector}/merge"),
            "--method".to_string(),
            "PUT".to_string(),
            "-f".to_string(),
            format!("sha={reviewed_head}"),
            "-f".to_string(),
            format!("merge_method={strategy}"),
        ],
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
    )?;
    if report_base_modified && is_base_modified_refusal(&result) {
        return Ok(json!({
            "merged": false,
            "refusal": BASE_MODIFIED_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    let result = succeeded(result, "reviewed PR merge")?;
    let response: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!("reviewed PR merge returned invalid JSON: {error}"))
    })?;
    if response.get("merged").and_then(Value::as_bool) != Some(true) {
        return Err(OrbitError::Execution(
            "reviewed PR merge did not confirm a synchronous merge; deferred merges are unsupported".into(),
        ));
    }
    let landed_commit = required_string(&response, "sha")?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
        "landed_commit": landed_commit,
    }))
}

/// True only for a completed `gh api` failure whose whole diagnostic is the
/// provider's base-modification refusal with HTTP 405: stderr is exactly
/// `gh: <reason> (HTTP 405)` and the response body, when present, is a JSON
/// error naming the same reason and status. Other 405s (policy, queue,
/// protection), other statuses, timeouts, and anything ambiguous do not match.
fn is_base_modified_refusal(result: &orbit_exec::ExecutionResult) -> bool {
    if result.success
        || result.timed_out
        || result.stderr.trim() != format!("gh: {BASE_MODIFIED_REASON} (HTTP 405)")
    {
        return false;
    }
    let body = result.stdout.trim();
    if body.is_empty() {
        return true;
    }
    let Ok(Value::Object(body)) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    body.get("message").and_then(Value::as_str) == Some(BASE_MODIFIED_REASON)
        && body
            .get("status")
            .is_none_or(|status| status.as_str() == Some("405") || status.as_u64() == Some(405))
}

/// Read the repository merge methods and the target branch's linear-history
/// rule in one GraphQL snapshot. Completion resolves a method from this live
/// state before asking GitHub to merge; it never mutates repository settings.
fn pr_merge_capabilities(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let pr_number = pr_number_from_selector(selector).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        ))
    })?;

    let repository = execute(
        "gh",
        vec![
            "repo".to_string(),
            "view".to_string(),
            "--json".to_string(),
            "nameWithOwner".to_string(),
        ],
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "repository identity",
    )?;
    let repository: Value = serde_json::from_str(&repository.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS repository identity returned invalid JSON: {error}"
        ))
    })?;
    let name_with_owner = repository
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS repository identity omitted nameWithOwner".to_string(),
            )
        })?;
    let (owner, name) = name_with_owner.split_once('/').ok_or_else(|| {
        OrbitError::Execution(format!(
            "private automation VCS repository identity '{name_with_owner}' is not owner/name"
        ))
    })?;

    let query = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){autoMergeAllowed mergeCommitAllowed rebaseMergeAllowed squashMergeAllowed pullRequest(number:$number){baseRefName baseRef{branchProtectionRule{requiresLinearHistory}}}}}";
    let response = execute(
        "gh",
        vec![
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={query}"),
            "-F".to_string(),
            format!("owner={owner}"),
            "-F".to_string(),
            format!("name={name}"),
            "-F".to_string(),
            format!("number={pr_number}"),
        ],
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "merge capabilities",
    )?;
    let response: Value = serde_json::from_str(&response.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS merge capabilities returned invalid JSON: {error}"
        ))
    })?;
    normalize_merge_capabilities(&response, name_with_owner)
}

fn normalize_merge_capabilities(
    response: &Value,
    name_with_owner: &str,
) -> Result<Value, OrbitError> {
    let repository = response
        .pointer("/data/repository")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities omitted data.repository".to_string(),
            )
        })?;
    let pull_request = repository
        .get("pullRequest")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities could not resolve the pull request"
                    .to_string(),
            )
        })?;
    let required_bool = |key: &str| {
        repository.get(key).and_then(Value::as_bool).ok_or_else(|| {
            OrbitError::Execution(format!(
                "private automation VCS merge capabilities omitted boolean {key}"
            ))
        })
    };
    let base_branch = pull_request
        .get("baseRefName")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities omitted the PR base branch".to_string(),
            )
        })?;
    let base_ref = pull_request.get("baseRef").ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS merge capabilities omitted PR baseRef policy data".to_string(),
        )
    })?;
    if base_ref.is_null() {
        return Err(OrbitError::Execution(
            "private automation VCS merge capabilities could not resolve the PR base ref"
                .to_string(),
        ));
    }
    let branch_protection_rule = base_ref.get("branchProtectionRule").ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS merge capabilities omitted branchProtectionRule policy data"
                .to_string(),
        )
    })?;
    let requires_linear_history = if branch_protection_rule.is_null() {
        false
    } else {
        branch_protection_rule
            .get("requiresLinearHistory")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "private automation VCS merge capabilities omitted boolean requiresLinearHistory"
                        .to_string(),
                )
            })?
    };

    Ok(json!({
        "repository": {
            "name_with_owner": name_with_owner,
            "base_branch": base_branch,
            "allow_squash_merge": required_bool("squashMergeAllowed")?,
            "allow_rebase_merge": required_bool("rebaseMergeAllowed")?,
            "allow_merge_commit": required_bool("mergeCommitAllowed")?,
            "allow_auto_merge": required_bool("autoMergeAllowed")?,
            "requires_linear_history": requires_linear_history,
        }
    }))
}

/// Read the merge-relevant state of a PR.
///
/// Separate from [`pr_view`], whose field set is fixed to the review-body
/// concerns its callers need. Completion asks a different question — is this PR
/// actually merged, and if not, what is holding it — so it selects the merge
/// state fields instead of widening a shared projection.
fn pr_status(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !valid_pr_selector(selector) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        )));
    }
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        selector.to_string(),
        "--json".to_string(),
        "number,state,mergedAt,mergeable,mergeStateStatus,statusCheckRollup,reviewDecision,headRefName,headRefOid,baseRefName,mergeCommit,url".to_string(),
    ];
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR status",
    )?;
    let pull_request: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR status returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": pull_request }))
}

/// Retry a private automation VCS read-only PR lookup (`PR_LIST`, `PR_VIEW`,
/// or `PR_STATUS`) across a bounded number of attempts when GitHub answers with
/// a transient failure. `pr_open` calls the first two operations to check for
/// an existing PR before deciding whether to create one; `PR_STATUS` is used
/// during completion. Pushes have their own remote-confirming retry path;
/// PR creation and merge are never retried here, since resending those mutations
/// after an ambiguous failure risks a duplicate side effect.
fn execute_with_transient_retry(
    program: &str,
    args: &[String],
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let mut attempt = 1;
    loop {
        match execute(program, args.to_vec(), current_dir, timeout_ms, operation) {
            Ok(result) => return Ok(result),
            Err(error)
                if attempt < GITHUB_LOOKUP_TRANSIENT_ATTEMPTS
                    && is_transient_github_lookup_failure(&error.to_string()) =>
            {
                tracing::warn!(
                    operation,
                    attempt,
                    "retrying private automation VCS lookup after a transient GitHub failure"
                );
                std::thread::sleep(GITHUB_LOOKUP_TRANSIENT_RETRY_DELAY);
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// True when a private automation VCS failure looks like a transient GitHub
/// gateway or GraphQL failure (502/503/504, a request timeout, or GitHub's
/// generic "Something went wrong while executing your query" response) rather
/// than a permanent failure such as auth, an unknown head, or an invalid
/// selector. Permanent failures must fail on the first attempt.
fn is_transient_github_lookup_failure(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("http 502")
        || text.contains("http 503")
        || text.contains("http 504")
        || text.contains("we couldn't respond to your request in time")
        || (text.contains("graphql")
            && text.contains("something went wrong while executing your query"))
        || (text.contains("graphql") && text.contains("timeout"))
}

fn execute(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    succeeded(
        run_vcs_process(program, args, current_dir, timeout_ms)?,
        operation,
    )
}

/// Run one VCS process and return its outcome, failed or not.
fn run_vcs_process(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let request = if program == "git" {
        let root = current_dir.ok_or_else(|| {
            OrbitError::InvalidInput("Git operation requires a working directory".to_string())
        })?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        super::git::git_request(root, &args, timeout_ms)
    } else {
        ExecRequest {
            program: program.to_string(),
            args,
            current_dir: current_dir.map(|path| path.to_string_lossy().into_owned()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(super::git::vcs_environment(&[
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
                "GH_HOST",
                "GH_CONFIG_DIR",
                "XDG_CONFIG_HOME",
            ])),
            debug: false,
        }
    };
    run_process(&request, &NoSandbox)
}

fn succeeded(
    result: orbit_exec::ExecutionResult,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    if !result.success {
        return Err(OrbitError::Execution(format!(
            "private automation VCS {operation} failed: {}",
            result.stderr.trim()
        )));
    }
    Ok(result)
}

fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str, OrbitError> {
    optional_string(input, key).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "private automation VCS operation requires non-empty '{key}' metadata"
        ))
    })
}

fn optional_string<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn reject_option_like(label: &str, value: &str) -> Result<(), OrbitError> {
    if value.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS {label} must not start with '-'"
        )));
    }
    Ok(())
}

fn valid_expected_remote_sha(value: Option<&str>) -> bool {
    value.is_some_and(|sha| {
        matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn valid_pr_selector(value: &str) -> bool {
    pr_number_from_selector(value).is_some()
}

fn pr_number_from_selector(value: &str) -> Option<&str> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(value);
    }
    if !value.contains("github.com/") || !value.contains("/pull/") {
        return None;
    }
    value
        .rsplit('/')
        .next()
        .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}
