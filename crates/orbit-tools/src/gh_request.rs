//! The `gh` request primitives every GitHub caller in this crate shares: the
//! inherited-environment `gh` invocation and the reader that types GitHub's
//! authenticated throttling answer [ORB-20178].
//!
//! A crate-root leaf, so `github_quota` builds and classifies `gh` requests
//! without naming the `builtin` GitHub tools that call into it [ORB-26098].
//! Only this module reads GitHub's rendered throttling diagnostic; the
//! GitHub tools' read check (`check_read_result`) types it from here.

use orbit_common::ProviderReadThrottle;
use orbit_exec::{EnvironmentMode, ExecRequest, StdinMode};

/// The longest header value or account identifier [`read_throttle`] keeps.
const MAX_TOKEN_BYTES: usize = 64;

/// A `gh` invocation inheriting this process's environment, with `stdin`
/// closed.
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

/// The throttle a rendered `gh` diagnostic reports, or `None` when it is any
/// other failure. Header lines (`Retry-After`, `X-RateLimit-Reset`,
/// `X-RateLimit-Resource`, `X-GitHub-Request-Id`) are read only when the
/// diagnostic carries them; an absent one stays unknown.
pub(crate) fn read_throttle(diagnostic: &str) -> Option<ProviderReadThrottle> {
    let lower = diagnostic.to_ascii_lowercase();
    if !(has_status(&lower, "403") || has_status(&lower, "429")) {
        return None;
    }
    let principal = ["user ID ", "installation ID "]
        .iter()
        .find_map(|kind| limited_account(diagnostic, kind));
    if principal.is_none() && !lower.contains("secondary rate limit") {
        return None;
    }
    let mut throttle = ProviderReadThrottle {
        principal,
        ..ProviderReadThrottle::default()
    };
    for line in diagnostic.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "retry-after" => throttle.retry_after_seconds = value.parse().ok(),
            "x-ratelimit-reset" => throttle.reset_epoch_seconds = value.parse().ok(),
            "x-ratelimit-resource" => throttle.resource = token(value),
            "x-github-request-id" => throttle.request_id = token(value),
            _ => {}
        }
    }
    Some(throttle)
}

/// `HTTP <status>` as a whole status token.
fn has_status(lower: &str, status: &str) -> bool {
    lower.match_indices("http ").any(|(index, prefix)| {
        let rest = &lower[index + prefix.len()..];
        rest.starts_with(status)
            && rest
                .as_bytes()
                .get(status.len())
                .is_none_or(|byte| !byte.is_ascii_digit())
    })
}

/// `user ID 123` from `API rate limit exceeded for user ID 123.`.
fn limited_account(diagnostic: &str, kind: &str) -> Option<String> {
    let (_, rest) = diagnostic.split_once(&format!("API rate limit exceeded for {kind}"))?;
    let digits = rest
        .bytes()
        .take_while(u8::is_ascii_digit)
        .take(MAX_TOKEN_BYTES)
        .count();
    (digits > 0).then(|| format!("{kind}{}", &rest[..digits]))
}

/// A bounded header token; anything else is unknown, never guessed.
fn token(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_:.".contains(&byte)))
    .then(|| value.to_string())
}
