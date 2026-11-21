//! What a `gh` request spends, whose quota it spends, and the rate-limit
//! endpoint read that refreshes the record.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::store::{ObservedQuota, RESOURCE_CORE, RESOURCE_GRAPHQL, RateLimitAnswer};

/// How long one rate-limit endpoint read may take.
const REFRESH_TIMEOUT_MS: u64 = 15_000;
/// GitHub's quota for a caller without a credential. An answer this small is
/// the address's quota, not the account's.
const UNAUTHENTICATED_LIMIT: u64 = 60;

/// The rate-limit resource `request` spends, or `None` when it is not a `gh`
/// call against the API: `auth`, `version`, `config` and `help` stay local or
/// probe the credential, and the rate-limit endpoint itself is free.
pub(crate) fn request_resource(request: &ExecRequest) -> Option<&'static str> {
    let program = Path::new(&request.program).file_stem()?.to_str()?;
    if program != "gh" {
        return None;
    }
    let mut args = request.args.iter().map(String::as_str);
    match args.next()? {
        "auth" | "version" | "--version" | "config" | "help" | "alias" | "completion" => None,
        "api" => {
            let rest = args.collect::<Vec<_>>();
            if rest.iter().any(|arg| is_endpoint(arg, "graphql")) {
                Some(RESOURCE_GRAPHQL)
            } else if rest.iter().any(|arg| is_endpoint(arg, "rate_limit")) {
                None
            } else {
                Some(RESOURCE_CORE)
            }
        }
        // `gh pr`, `gh issue` and `gh repo` read and write through GraphQL.
        "pr" | "issue" | "repo" | "project" => Some(RESOURCE_GRAPHQL),
        _ => Some(RESOURCE_CORE),
    }
}

fn is_endpoint(arg: &str, endpoint: &str) -> bool {
    arg.trim_start_matches('/') == endpoint
}

/// An opaque record key for the GitHub host and credential `request` uses:
/// the host name and a truncated SHA-256 of the token from the request's
/// environment, or of the config location `gh` reads its stored login from.
/// The credential itself never reaches the record.
pub(crate) fn request_account(request: &ExecRequest) -> String {
    let lookup = |name: &str| -> Option<String> {
        let value = match &request.environment_mode {
            EnvironmentMode::Inherit => std::env::var(name).ok(),
            EnvironmentMode::ClearAndSet(pairs) => pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone()),
        };
        value.filter(|value| !value.trim().is_empty())
    };
    let host = lookup("GH_HOST").unwrap_or_else(|| "github.com".to_string());
    let host = host
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    let token_names: &[&str] = if host == "github.com" {
        &["GH_TOKEN", "GITHUB_TOKEN"]
    } else {
        &[
            "GH_ENTERPRISE_TOKEN",
            "GITHUB_ENTERPRISE_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
        ]
    };
    let credential = token_names
        .iter()
        .find_map(|name| lookup(name))
        .map(|token| format!("token:{token}"))
        .or_else(|| lookup("GH_CONFIG_DIR").map(|dir| format!("config:{dir}")))
        .or_else(|| lookup("XDG_CONFIG_HOME").map(|dir| format!("xdg:{dir}")))
        .or_else(|| lookup("HOME").map(|dir| format!("home:{dir}")))
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(host.as_bytes());
    hasher.update([0]);
    hasher.update(credential.as_bytes());
    let digest = hasher.finalize();
    let fingerprint = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{host}-{fingerprint}")
}

/// Build the `gh api rate_limit` probe request for `request`, bounded by
/// `request.timeout_ms` (and at most [`REFRESH_TIMEOUT_MS`]). Returns `None`
/// when `request`'s finite timeout budget is already zero. A probe that the
/// caller's budget cuts short is answered as `Clipped`, which leaves the shared
/// record as it was.
pub(crate) fn rate_limit_probe_request(request: &ExecRequest) -> Option<ExecRequest> {
    let timeout_ms = match request.timeout_ms {
        Some(0) => return None,
        Some(limit) => limit.min(REFRESH_TIMEOUT_MS),
        None => REFRESH_TIMEOUT_MS,
    };
    Some(ExecRequest {
        program: request.program.clone(),
        args: vec!["api".to_string(), "rate_limit".to_string()],
        current_dir: request.current_dir.clone(),
        timeout_ms: Some(timeout_ms),
        stdin_mode: StdinMode::Null,
        environment_mode: request.environment_mode.clone(),
        debug: false,
    })
}

/// Read GitHub's rate-limit endpoint with `request`'s program, environment
/// and directory, so it answers for the same account. The endpoint does not
/// count against the quota.
///
/// With a `deadline`, the probe gets what is left of it when the read starts,
/// not `request`'s original budget; an expired deadline skips the read.
pub(crate) fn read_rate_limit(request: &ExecRequest, deadline: Option<Instant>) -> RateLimitAnswer {
    let remaining;
    let request = match deadline {
        Some(deadline) => {
            remaining = ExecRequest {
                timeout_ms: Some(super::remaining_millis(deadline)),
                ..request.clone()
            };
            &remaining
        }
        None => request,
    };
    let Some(probe) = rate_limit_probe_request(request) else {
        return RateLimitAnswer::Clipped(
            "rate-limit read skipped: timeout budget exhausted".to_string(),
        );
    };
    let clipped = probe
        .timeout_ms
        .is_some_and(|limit| limit < REFRESH_TIMEOUT_MS);
    match run_rate_limit_probe(&probe, clipped, deadline) {
        Ok(result) if result.success => parse_rate_limit(&result.stdout),
        // The caller's deadline, not the endpoint, ended the read.
        Ok(result) if result.timed_out && clipped => RateLimitAnswer::Clipped(
            "rate-limit read cut short by the caller's timeout budget".to_string(),
        ),
        Ok(result) => RateLimitAnswer::Unknown(format!(
            "rate-limit read failed: {}",
            orbit_common::security::redaction::redact_all(result.stderr.trim())
        )),
        Err(error) => RateLimitAnswer::Unknown(format!(
            "rate-limit read could not run: {}",
            orbit_common::security::redaction::redact_all(&error.to_string())
        )),
    }
}

/// Run the rate-limit probe. A fixture inside `with_probe_timeout_when`
/// moves a clipped probe's timeout from its wall-clock budget to an event.
fn run_rate_limit_probe(
    probe: &ExecRequest,
    clipped: bool,
    deadline: Option<Instant>,
) -> Result<orbit_exec::ExecutionResult, orbit_common::OrbitError> {
    #[cfg(any(test, feature = "test-support"))]
    if clipped && let Some(fired) = probe_timeout::armed() {
        return probe_timeout::run(probe, deadline, fired.as_ref());
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (clipped, deadline);
    run_process(probe, &NoSandbox)
}

/// Run `f` with this thread's clipped rate-limit probes timing out when
/// `fired` first returns true, instead of when their wall-clock budget runs
/// out. Test fixtures only.
///
/// A fixture that gives a request a short budget and a `gh` that hangs on
/// `api rate_limit` means "the budget ends while the probe is running". With
/// the real timer, a loaded host can kill the probe before the fake `gh` has
/// started, or has logged its call (ORB-26200). Pass a `fired` that turns true
/// once the fake has logged the call: the probe is then stopped, reported as
/// timed out, and this thread's admission deadline clock is moved to the
/// request's deadline, which is where the real timeout leaves both. The probe
/// keeps its production arguments, environment and supervision, and the
/// endpoint's 15 s read limit still bounds a `fired` that never turns true.
#[cfg(any(test, feature = "test-support"))]
pub fn with_probe_timeout_when<T>(
    fired: impl Fn() -> bool + Send + Sync + 'static,
    f: impl FnOnce() -> T,
) -> T {
    let _armed = probe_timeout::arm(std::sync::Arc::new(fired));
    f()
}

#[cfg(any(test, feature = "test-support"))]
mod probe_timeout {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use orbit_common::OrbitError;
    use orbit_exec::{ExecRequest, ExecutionResult, NoSandbox, Sandbox, StdinMode};

    use super::REFRESH_TIMEOUT_MS;

    pub(super) type Fired = Arc<dyn Fn() -> bool + Send + Sync>;

    thread_local! {
        static FIRED: RefCell<Option<Fired>> = const { RefCell::new(None) };
    }

    /// How often the watcher checks `fired`. It only bounds how soon the
    /// probe stops after the event; no outcome depends on it.
    const POLL: Duration = Duration::from_millis(1);

    pub(super) struct Armed(Option<Fired>);

    impl Drop for Armed {
        fn drop(&mut self) {
            let previous = self.0.take();
            FIRED.with(|slot| *slot.borrow_mut() = previous);
        }
    }

    pub(super) fn arm(fired: Fired) -> Armed {
        Armed(FIRED.with(|slot| slot.borrow_mut().replace(fired)))
    }

    pub(super) fn armed() -> Option<Fired> {
        FIRED.with(|slot| slot.borrow().clone())
    }

    pub(super) fn run(
        probe: &ExecRequest,
        deadline: Option<Instant>,
        fired: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<ExecutionResult, OrbitError> {
        NoSandbox.validate(probe)?;
        let stop = AtomicBool::new(false);
        let done = AtomicBool::new(false);
        let stdin_payload = match &probe.stdin_mode {
            StdinMode::Bytes(bytes) => Some(bytes.clone()),
            StdinMode::Inherit | StdinMode::Null => None,
        };
        let started = Instant::now();
        let outcome = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    if fired() {
                        stop.store(true, Ordering::Release);
                        return;
                    }
                    std::thread::sleep(POLL);
                }
            });
            let outcome = orbit_exec::spawn_supervised_cancellable(
                || NoSandbox.spawn(probe),
                Some(REFRESH_TIMEOUT_MS),
                stdin_payload,
                Some(&stop),
            );
            done.store(true, Ordering::Release);
            outcome
        })?;
        let mut result = outcome.result;
        if stop.load(Ordering::Acquire) && !result.success {
            // The event stands in for the deadline: the probe timed out, and
            // the admission clock reads what it reads at the real timeout.
            result.timed_out = true;
            result.duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if let Some(deadline) = deadline {
                super::super::store::reach_deadline(deadline);
            }
        }
        Ok(result)
    }
}

/// The tracked resources of a `GET /rate_limit` body. Anything else, and an
/// unauthenticated answer, is unknown.
pub(crate) fn parse_rate_limit(body: &str) -> RateLimitAnswer {
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return RateLimitAnswer::Unknown("rate-limit answer is not JSON".to_string());
    };
    let mut observed = BTreeMap::new();
    for name in [RESOURCE_CORE, RESOURCE_GRAPHQL] {
        let Some(entry) = parsed.pointer(&format!("/resources/{name}")) else {
            continue;
        };
        let field = |key: &str| entry.get(key).and_then(Value::as_u64);
        if let (Some(limit), Some(remaining), Some(reset)) =
            (field("limit"), field("remaining"), field("reset"))
        {
            observed.insert(
                name.to_string(),
                ObservedQuota {
                    limit,
                    remaining,
                    reset_epoch_seconds: reset,
                },
            );
        }
    }
    match observed.get(RESOURCE_CORE) {
        None => RateLimitAnswer::Unknown("rate-limit answer names no core quota".to_string()),
        Some(core) if core.limit <= UNAUTHENTICATED_LIMIT => RateLimitAnswer::Unknown(
            "rate-limit answer is for an unauthenticated caller".to_string(),
        ),
        Some(_) => RateLimitAnswer::Known(observed),
    }
}
