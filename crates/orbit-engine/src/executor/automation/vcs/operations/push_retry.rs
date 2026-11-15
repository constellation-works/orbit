use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_common::process::jitter::JitterRng;
use orbit_common::{ForgePushHoldFailure, OrbitError, PrivateVcsFailure, PrivateVcsFailureClass};
use orbit_types::workflow::ForgeUnavailableHold;
use serde_json::{Value, json};

use super::super::git::direct_remote_target;
use super::input::{
    optional_string, reject_option_like, required_string, valid_expected_remote_sha,
};
use super::process::{execute, run_vcs_process, succeeded};
use super::{CANDIDATE_REF_PREFIX, DEFAULT_TIMEOUT_MS, LONG_TIMEOUT_MS};

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

pub(super) fn push(input: &Value) -> Result<Value, OrbitError> {
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
pub(super) fn push_candidate_ref(input: &Value) -> Result<Value, OrbitError> {
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
pub(super) struct PushBackoff {
    attempts: u32,
    initial_ms: u64,
    cap_ms: u64,
    window_ms: Option<u64>,
}

impl PushBackoff {
    pub(super) fn from_input(input: &Value) -> Result<Self, OrbitError> {
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
pub(super) struct Pushed {
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
pub(super) enum PushFailure {
    /// Not retried: a permanent refusal, an unconfirmed timeout, or a failed
    /// confirmation read.
    Error(OrbitError),
    /// Every attempt in the budget was refused for a transient reason.
    Exhausted(Box<PushExhausted>),
}

pub(super) struct PushExhausted {
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
    pub(super) fn into_error(self) -> OrbitError {
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
        let message = hold.text(&format!(
            "the forge refused the push {} times over {} s: {}",
            exhausted.attempts,
            exhausted.waited_ms / 1000,
            exhausted.error
        ));
        OrbitError::ForgeUnavailable(Box::new(ForgePushHoldFailure { hold, message }))
    }
}

/// Arguments for one push attempt. After a `missing necessary objects`
/// refusal, later attempts pass `--no-thin` so the pack does not delta
/// against bases the server advertised and then did not have.
fn push_attempt_args(args: &[String], no_thin: bool) -> Vec<String> {
    if !no_thin {
        return args.to_vec();
    }
    let Some((command, rest)) = args.split_first() else {
        return Vec::new();
    };
    let mut attempt_args = Vec::with_capacity(args.len() + 1);
    attempt_args.push(command.clone());
    if command == "push" {
        attempt_args.push("--no-thin".to_string());
    }
    attempt_args.extend(rest.iter().cloned());
    attempt_args
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

/// What a publication read showed for the pushed ref.
enum PublicationObservation {
    /// Every effective push URL has `head_sha` at `target_ref`.
    Landed(orbit_exec::ExecutionResult),
    /// The destination has no such ref.
    Absent,
    /// The ref is present and names a different commit.
    OtherSha(String),
}

/// How one remote's `ls-remote` output names `target_ref`.
enum RefTip {
    Exact,
    Absent,
    OtherSha(String),
}

/// Whether every effective publication destination holds `head_sha` at
/// `target_ref`. A failed remote read is an error, so it stops further
/// mutations instead of reading as an unconfirmed push. Anything other than
/// an exact match reports why, so a timed-out push can name the miss without
/// a second probe.
fn confirmed_at_publication_destination(
    repo_root: &Path,
    target_ref: &str,
    head_sha: &str,
) -> Result<PublicationObservation, OrbitError> {
    let listed = execute(
        "git",
        vec![
            "remote".into(),
            "get-url".into(),
            "--push".into(),
            "--all".into(),
            "origin".into(),
        ],
        Some(repo_root),
        LONG_TIMEOUT_MS,
        "push remote confirmation",
    )?;
    let mut urls: Vec<String> = Vec::new();
    for url in listed
        .stdout
        .lines()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        if !urls.iter().any(|seen| seen == url) {
            urls.push(url.to_string());
        }
    }
    if urls.is_empty() {
        return Err(OrbitError::Execution(
            "private automation VCS push remote confirmation found no effective push URL for 'origin'"
                .to_string(),
        ));
    }
    let mut confirmation = None;
    for url in &urls {
        let target = direct_remote_target(url);
        let mut verify_args = target.prefix_args();
        verify_args.extend([
            "ls-remote".into(),
            "--get-url".into(),
            target.probe_url.clone(),
        ]);
        let expanded = execute(
            "git",
            verify_args,
            Some(repo_root),
            LONG_TIMEOUT_MS,
            "push remote confirmation",
        )?;
        if expanded.stdout.trim() != url {
            return Err(OrbitError::Execution(format!(
                "git_push: effective push URL '{url}' would be rewritten to '{}' when probed \
                 by URL, so it does not name the repository 'git push origin' writes; \
                 refusing to confirm publication at a different repository",
                expanded.stdout.trim()
            )));
        }
        let mut args = target.prefix_args();
        args.extend([
            "ls-remote".into(),
            "--refs".into(),
            "--".into(),
            target.probe_url.clone(),
            target_ref.into(),
        ]);
        let remote = execute(
            "git",
            args,
            Some(repo_root),
            LONG_TIMEOUT_MS,
            "push remote confirmation",
        )?;
        match observed_ref_tip(&remote.stdout, target_ref, head_sha) {
            RefTip::Exact => {
                if confirmation.is_none() {
                    confirmation = Some(remote);
                }
            }
            RefTip::Absent => return Ok(PublicationObservation::Absent),
            RefTip::OtherSha(sha) => return Ok(PublicationObservation::OtherSha(sha)),
        }
    }
    match confirmation {
        Some(result) => Ok(PublicationObservation::Landed(result)),
        None => Err(OrbitError::Execution(
            "private automation VCS push remote confirmation found no effective push URL for 'origin'"
                .to_string(),
        )),
    }
}

/// The tip `stdout` names for `target_ref`. An exact `head_sha` match wins
/// over any other line for that ref.
fn observed_ref_tip(stdout: &str, target_ref: &str, head_sha: &str) -> RefTip {
    let mut other = None;
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let Some(sha) = fields.next() else {
            continue;
        };
        if fields.next() != Some(target_ref) || fields.next().is_some() {
            continue;
        }
        if sha.eq_ignore_ascii_case(head_sha) {
            return RefTip::Exact;
        }
        if other.is_none() {
            other = Some(sha.to_string());
        }
    }
    match other {
        Some(sha) => RefTip::OtherSha(sha),
        None => RefTip::Absent,
    }
}

/// A timed-out push that the publication read did not show as landed.
/// Permanent: the push is not sent again, and a later transient retry must
/// not treat the confirmation detail as a reason to resend it.
fn timed_out_confirmation_error(
    operation: &str,
    target_ref: &str,
    head_sha: &str,
    observation: Result<PublicationObservation, OrbitError>,
) -> OrbitError {
    let outcome = match observation {
        Ok(PublicationObservation::Absent) => "confirmation absent".to_string(),
        Ok(PublicationObservation::OtherSha(sha)) => {
            format!("confirmation at another sha {sha}")
        }
        Ok(PublicationObservation::Landed(_)) => {
            "confirmation unreadable: landed observation was not accepted".to_string()
        }
        Err(error) => {
            let detail = error.to_string().replace(['\n', '\r'], " ");
            format!("confirmation unreadable: {detail}")
        }
    };
    let message = format!(
        "private automation VCS {operation} failed: process timed out pushing commit {head_sha} \
         to {target_ref}; {outcome}"
    );
    OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
        operation: operation.to_string(),
        class: PrivateVcsFailureClass::Other,
        message,
        read_throttle: None,
    }))
}

/// One probe after a timed-out push. A lost reply may hide a push that
/// landed; an exact ref is success, and any other outcome fails without
/// sending the push again.
fn confirm_timed_out_push(
    repo_root: &Path,
    target_ref: &str,
    head_sha: &str,
    operation: &str,
    attempt: u32,
    waited_ms: u64,
) -> Result<Pushed, PushFailure> {
    let observation = confirmed_at_publication_destination(repo_root, target_ref, head_sha);
    match observation {
        Ok(PublicationObservation::Landed(confirmation)) => {
            tracing::info!(target: "orbit_engine::executor::automation::vcs::operations",
                operation,
                attempt,
                waited_ms,
                target_ref,
                head_sha,
                "confirmed push landed after a timeout"
            );
            Ok(Pushed {
                result: confirmation,
                attempts: attempt,
                waited_ms,
            })
        }
        other => {
            tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
                operation,
                attempt,
                waited_ms,
                target_ref,
                head_sha,
                "timed-out push was not confirmed at the publication destination"
            );
            Err(PushFailure::Error(timed_out_confirmation_error(
                operation, target_ref, head_sha, other,
            )))
        }
    }
}

/// Retry only transient push failures, confirming the exact remote ref before
/// resending. A lost reply may hide a successful push, including one that
/// consumed its force-with-lease. A timeout is confirmed once the same way
/// and is not retried: an exact ref is a landed push, and any other outcome
/// fails. A failed remote read stops further mutations.
pub(super) fn execute_push_with_transient_retry(
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
    // Set once this push is refused for missing thin-pack bases. Later
    // attempts then send `--no-thin` and no longer depend on those bases.
    let mut no_thin = false;
    loop {
        let result = run_vcs_process(
            "git",
            push_attempt_args(args, no_thin),
            Some(repo_root),
            LONG_TIMEOUT_MS,
        )
        .map_err(PushFailure::Error)?;
        let diagnostic = format!("{}\n{}", result.stdout, result.stderr);
        if result.timed_out {
            return confirm_timed_out_push(
                repo_root, target_ref, head_sha, operation, attempt, waited_ms,
            );
        }
        if result.success || !is_transient_push_failure(&diagnostic) {
            return succeeded(result, operation)
                .map(|result| Pushed {
                    result,
                    attempts: attempt,
                    waited_ms,
                })
                .map_err(PushFailure::Error);
        }
        if rejected_for_missing_necessary_objects(&diagnostic) {
            no_thin = true;
        }

        // Confirm at the publication destination `git push origin`
        // wrote, not the fetch remote: a plain `ls-remote origin` reads
        // the fetch URL, and probing the resolved push URL by URL would
        // apply `insteadOf` to it a second time [ORB-17671, ORB-17686].
        if let PublicationObservation::Landed(confirmation) =
            confirmed_at_publication_destination(repo_root, target_ref, head_sha)
                .map_err(PushFailure::Error)?
        {
            tracing::info!(target: "orbit_engine::executor::automation::vcs::operations",
                operation,
                attempt,
                waited_ms,
                "confirmed push landed after a transient failure"
            );
            return Ok(Pushed {
                result: confirmation,
                attempts: attempt,
                waited_ms,
            });
        }
        let (first_refused, first_refused_at) =
            *first_refusal.get_or_insert_with(|| (Instant::now(), Utc::now()));
        if backoff.spent(attempt, first_refused.elapsed()) {
            tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
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
        tracing::warn!(target: "orbit_engine::executor::automation::vcs::operations",
            operation,
            attempt,
            delay_ms,
            waited_ms,
            target_ref,
            head_sha,
            no_thin,
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
    // GitHub's internal ref-update failures use `(failure)`, which is too
    // generic to retry without the accompanying remote fatal diagnostic.
    // The fatal line is separate from the ref rejection in Git's output.
    let internal_commit_refs_failure = text.lines().any(|line| {
        matches!(
            line.trim(),
            "remote: fatal error in commit_refs" | "remote: fatal error in commit_refs_rules"
        )
    });
    // GitHub names a server-side fault in the rejection's reason, as in
    // `! [remote rejected] head -> head (Internal Server Error)` [ORB-14617].
    text.lines().any(|line| {
        let line = line.trim_end();
        line.contains("[remote rejected]")
            && ((internal_commit_refs_failure && line.ends_with("(failure)"))
                || [
                    "(failed)",
                    "(internal server error)",
                    "(service unavailable)",
                    "(bad gateway)",
                    "(gateway timeout)",
                    // A thin pack whose delta bases the server no longer has.
                    // The retry sends `--no-thin` [ORB-16529].
                    "(missing necessary objects)",
                ]
                .iter()
                .any(|reason| line.ends_with(reason)))
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

/// A `[remote rejected] … (missing necessary objects)` line. Permanent
/// diagnostics are decided first by [`is_transient_push_failure`].
fn rejected_for_missing_necessary_objects(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.lines().any(|line| {
        let line = line.trim_end();
        line.contains("[remote rejected]") && line.ends_with("(missing necessary objects)")
    })
}

/// Whether `value` is a ref under [`CANDIDATE_REF_PREFIX`] made of plain
/// path segments, so it can be neither an option nor a refspec.
pub(crate) fn valid_candidate_ref(value: &str) -> bool {
    value
        .strip_prefix(CANDIDATE_REF_PREFIX)
        .is_some_and(plain_ref_segments)
}

/// Whether the rest of a ref after its namespace is one or more plain path
/// segments: ASCII letters, digits, `-`, `_` and `.`, never `..`, a leading
/// `.` or a `.lock` suffix.
pub(super) fn plain_ref_segments(rest: &str) -> bool {
    !rest.is_empty()
        && rest.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with('.')
                && !segment.ends_with(".lock")
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
                && !segment.contains("..")
        })
}
