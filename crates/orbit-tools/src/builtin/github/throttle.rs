//! GitHub's authenticated throttling answer to a read [ORB-20178].
//!
//! Only this adapter reads GitHub's rendered diagnostic. It types the one
//! answer a caller's policy may wait out: an authenticated primary or
//! secondary rate limit. Everything else, including an ordinary 403, a 401,
//! a 404 and an unauthenticated limit (which names an address, not an
//! account, so the credential is missing), stays the untyped failure it was.
//! The rendered text is unchanged either way.

use orbit_common::{OrbitError, PrivateVcsFailure, PrivateVcsFailureClass, ProviderReadThrottle};
use orbit_types::tool::ExecutionResult;

use crate::check_exec_result;

/// The longest header value or account identifier this reader keeps.
const MAX_TOKEN_BYTES: usize = 64;

/// [`check_exec_result`] for a read, typing GitHub's authenticated throttling
/// answer so the caller can tell it from a refusal.
pub(super) fn check_read_result(result: &ExecutionResult, label: &str) -> Result<(), OrbitError> {
    check_exec_result(result, label).map_err(|error| match read_throttle(&result.stderr) {
        Some(throttle) => OrbitError::PrivateVcs(Box::new(PrivateVcsFailure {
            operation: label.to_string(),
            class: PrivateVcsFailureClass::Other,
            message: format!("{label} failed: {}", result.stderr.trim()),
            read_throttle: Some(throttle),
        })),
        None => error,
    })
}

/// The throttle `error` carries, when an adapter typed one.
pub(super) fn throttle_of(error: &OrbitError) -> Option<ProviderReadThrottle> {
    error
        .private_vcs_failure()
        .and_then(|failure| failure.read_throttle.clone())
}

/// The throttle a rendered `gh` diagnostic reports, or `None` when it is any
/// other failure. Header lines (`Retry-After`, `X-RateLimit-Reset`,
/// `X-RateLimit-Resource`, `X-GitHub-Request-Id`) are read only when the
/// diagnostic carries them; an absent one stays unknown.
fn read_throttle(diagnostic: &str) -> Option<ProviderReadThrottle> {
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
