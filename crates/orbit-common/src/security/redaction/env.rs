use std::{borrow::Cow, sync::OnceLock};

use serde_json::Value;

use super::error::redact_json_with;

const REDACTED_ENV_VALUE: &str = "[REDACTED_ENV]";
static SENSITIVE_ENV_VALUES: OnceLock<Vec<String>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Env-var value scrubbing
// ---------------------------------------------------------------------------

/// Replace occurrences of any sensitive env-var value (as seen in the live
/// process environment) with `[REDACTED_ENV]`.
///
/// "Sensitive" is matched against the var *name* — anything containing
/// SECRET / TOKEN / PASSWORD / API_KEY / etc. See [`is_sensitive_env_name`].
/// Only values that pass `is_redactable_value` are substituted, so the small
/// compatibility set of ordinary words held by a sensitive-named variable is
/// left untouched.
///
/// Each eligible value is also matched in its JSON-string body
/// (`serde_json::to_string` without the surrounding quotes) and its Rust
/// `Debug` body (`format!("{:?}", value)` without the surrounding quotes)
/// when that encoding differs from the raw text. Audit blobs and tracing
/// `?` fields contain those encodings, not the live value.
pub fn redact_sensitive_env_text(raw: &str) -> String {
    let mut redacted = raw.to_string();
    // `redact_all` runs this for every string field of every tracing event, so
    // only a value that actually occurs pays for a `replace` allocation.
    for secret in sensitive_env_values().iter() {
        if redacted.contains(secret.as_str()) {
            redacted = redacted.replace(secret.as_str(), REDACTED_ENV_VALUE);
        }
    }
    redacted
}

/// Replace complete sensitive environment values in `bytes`.
///
/// Returns how many trailing bytes the caller must keep. That suffix is the
/// longest proper prefix of a sensitive value that is also a suffix of the
/// redacted buffer, so a value split across chunks is still replaced once it
/// completes. Complete values are removed first, and the held suffix does not
/// cut through one that was already whole. Invalid UTF-8 is copied through
/// unchanged; only valid segments are matched as text.
pub fn redact_sensitive_env_bytes(bytes: &mut Vec<u8>) -> usize {
    if !bytes.is_empty() {
        redact_complete_env_values(bytes);
    }
    sensitive_value_holdback(bytes)
}

fn redact_complete_env_values(bytes: &mut Vec<u8>) {
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.extend_from_slice(redact_sensitive_env_text(text).as_bytes());
                break;
            }
            Err(err) => {
                let valid = err.valid_up_to();
                if let Ok(text) = std::str::from_utf8(&rest[..valid]) {
                    out.extend_from_slice(redact_sensitive_env_text(text).as_bytes());
                } else {
                    out.extend_from_slice(&rest[..valid]);
                }
                let Some(invalid) = err.error_len().filter(|len| *len > 0) else {
                    out.extend_from_slice(&rest[valid..]);
                    break;
                };
                let skip = valid + invalid;
                out.extend_from_slice(&rest[valid..skip]);
                rest = &rest[skip..];
            }
        }
    }
    *bytes = out;
}

/// Longest suffix of `text` that is a proper prefix of a sensitive value.
///
/// `0` when nothing is pending. A full value is not a holdback: callers redact
/// complete values before asking, so a border of a value that already occurred
/// is not reported as if the value were still incomplete.
fn sensitive_value_holdback(text: &[u8]) -> usize {
    let secrets = sensitive_env_values();
    let mut best = 0usize;
    for secret in secrets.iter() {
        best = best.max(proper_prefix_suffix_len(text, secret.as_bytes()));
    }
    best
}

/// Length of the longest proper prefix of `pat` that is a suffix of `text`.
fn proper_prefix_suffix_len(text: &[u8], pat: &[u8]) -> usize {
    if pat.len() < 2 || text.is_empty() {
        return 0;
    }
    let max = pat.len() - 1;
    let window = if text.len() > max {
        &text[text.len() - max..]
    } else {
        text
    };
    let lps = proper_prefix_table(pat);
    let mut state = 0usize;
    for &byte in window {
        while state > 0 && pat[state] != byte {
            state = lps[state - 1];
        }
        if pat[state] == byte {
            state += 1;
            if state == pat.len() {
                state = lps[state - 1];
            }
        }
    }
    state
}

fn proper_prefix_table(pat: &[u8]) -> Vec<usize> {
    let mut table = vec![0usize; pat.len()];
    let mut len = 0usize;
    let mut index = 1usize;
    while index < pat.len() {
        if pat[index] == pat[len] {
            len += 1;
            table[index] = len;
            index += 1;
        } else if len > 0 {
            len = table[len - 1];
        } else {
            table[index] = 0;
            index += 1;
        }
    }
    table
}

pub fn redact_sensitive_env_json(value: Value) -> Value {
    redact_json_with(value, redact_sensitive_env_text)
}

fn collect_sensitive_env_values() -> Vec<String> {
    let mut values = std::env::vars()
        .filter(|(name, value)| is_sensitive_env_name(name) && is_redactable_value(value))
        .flat_map(|(_, value)| sensitive_value_forms(value))
        .collect::<Vec<_>>();
    // Longest first so a raw value that sits inside its own encoding cannot
    // split that longer match before it is replaced.
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

/// Live value, plus the JSON-string and Rust `Debug` bodies when they differ.
///
/// `serde_json` and `Debug` turn newlines, quotes, and backslashes into
/// escape sequences. A blob or tracing field serialized before redaction no
/// longer contains the raw value, so those bodies have to be substitutes too.
/// Identical encodings are stored once.
fn sensitive_value_forms(value: String) -> Vec<String> {
    let mut forms = Vec::with_capacity(3);
    for encoded in [json_string_body(&value), debug_string_body(&value)]
        .into_iter()
        .flatten()
    {
        if encoded != value && !forms.contains(&encoded) {
            forms.push(encoded);
        }
    }
    forms.push(value);
    forms
}

fn json_string_body(value: &str) -> Option<String> {
    let encoded = serde_json::to_string(value).ok()?;
    quoted_body(&encoded).map(str::to_string)
}

fn debug_string_body(value: &str) -> Option<String> {
    quoted_body(&format!("{value:?}")).map(str::to_string)
}

/// Body of a quoted string encoding. The surrounding quotes are syntax, not
/// part of the value inside a serialized blob or debug field.
fn quoted_body(encoded: &str) -> Option<&str> {
    encoded.strip_prefix('"')?.strip_suffix('"')
}

fn cached_sensitive_env_values() -> &'static [String] {
    SENSITIVE_ENV_VALUES
        .get_or_init(collect_sensitive_env_values)
        .as_slice()
}

fn sensitive_env_values() -> Cow<'static, [String]> {
    // Tests mutate process env after startup (`EnvVarGuard`). A process-wide
    // snapshot would miss those values, so re-collect in this crate's tests
    // and while the shared scoped environment guard is active. Normal
    // production calls retain the cached snapshot.
    if cfg!(test) || crate::test_env::scoped_env_active() {
        Cow::Owned(collect_sensitive_env_values())
    } else {
        Cow::Borrowed(cached_sensitive_env_values())
    }
}

/// Decide whether a sensitive-named env value is eligible for substitution
/// by [`redact_sensitive_env_text`].
///
/// A value is eligible when, after trim, it is at least 4 characters and is
/// not one of the small compatibility set of ordinary words (`user`, `true`,
/// `false`, `none`, `null`, `root`, `main`, `test`, `prod`, `local`, `auto`).
/// This keeps those known prose/sentinel values readable while allowing
/// all-letter secrets and passphrases from sensitive credential variables to
/// be scrubbed.
/// Pure ASCII decimal values shorter than 12 digits are also excluded. Small
/// counters and session IDs recur inside unrelated hashes, IDs and timestamps;
/// substring substitution of these low-entropy values corrupts structured
/// replies. Twelve or more digits remain eligible, as do mixed credentials.
///
/// Eligible values are still matched with a bare substring replace so
/// embedded tokens in URLs and concatenated log fragments stay scrubbed.
///
/// Pattern-based redaction still independently catches provider-shaped tokens
/// (`ghp_…`, `sk-…`).
// pub(crate) for sibling-layout tests in security/tests/redaction.rs.
pub(crate) fn is_redactable_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.len() >= 4
        && !is_compatibility_ordinary_word(trimmed)
        && !(trimmed.len() < 12 && trimmed.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_compatibility_ordinary_word(value: &str) -> bool {
    [
        "user", "true", "false", "none", "null", "root", "main", "test", "prod", "local", "auto",
    ]
    .into_iter()
    .any(|word| value.eq_ignore_ascii_case(word))
}

/// Match credential names, excluding well-known non-credential session
/// metadata. Other `SESSION` names retain conservative handling for existing
/// callers, including child-environment filtering. Credential words take
/// precedence over the metadata exclusion, so session tokens and keys remain
/// sensitive even in the XDG namespace.
pub fn is_sensitive_env_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let session_metadata = upper.starts_with("XDG_SESSION_")
        || matches!(
            upper.as_str(),
            "DBUS_SESSION_BUS_ADDRESS" | "SESSION_MANAGER" | "TERM_SESSION_ID"
        );
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.contains("PASSWD")
        || upper.contains("PASSCODE")
        || upper.contains("API_KEY")
        || upper.ends_with("_KEY")
        || upper.contains("PRIVATE")
        || upper.contains("CREDENTIAL")
        || upper.contains("COOKIE")
        || (upper.contains("SESSION") && (upper.contains("KEY") || !session_metadata))
        || upper.contains("BEARER")
        || contains_auth_word(&upper)
}

/// True when `upper` (already uppercased) has an AUTH-family credential
/// segment, e.g. `AUTH_TOKEN`, `AUTHORIZATION`, `OAUTH`, or `XAUTH`.
///
/// A bare substring test also matches `GIT_AUTHOR_NAME` / `GIT_AUTHOR_EMAIL`
/// (ordinary git identity vars Orbit itself sets for child processes), which
/// are not credentials. Requiring an exact `AUTH` segment overshoots that
/// carve-out and misses `AUTH*` / `*AUTH` words other than `AUTHOR*`.
fn contains_auth_word(upper: &str) -> bool {
    upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(segment_is_auth_sensitive)
}

fn segment_is_auth_sensitive(segment: &str) -> bool {
    if matches!(segment, "AUTHOR" | "AUTHORS" | "AUTHORED") {
        return false;
    }
    segment.starts_with("AUTH") || segment.ends_with("AUTH")
}
