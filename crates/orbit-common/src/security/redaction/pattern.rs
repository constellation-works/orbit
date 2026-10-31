// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use std::{borrow::Cow, sync::OnceLock};

use regex::{Regex, RegexSet};

use super::env::redact_sensitive_env_text_cow;

static DEFAULT_PATTERN_REDACTOR: OnceLock<PatternRedactor> = OnceLock::new();
static ARGV_PATTERN_REDACTOR: OnceLock<PatternRedactor> = OnceLock::new();
static HIGH_CONFIDENCE_SINGLE_TOKEN_PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();

// ---------------------------------------------------------------------------
// Pattern-based redaction (HTTP + argv)
// ---------------------------------------------------------------------------

/// Regex-driven scrubber for credential shapes and infrastructure identifiers.
///
/// Builds to `default()` cover Authorization / x-api-key / URL key params /
/// Bearer / raw header lines, high-confidence provider credentials, and
/// structurally recognizable SSH fingerprints, public-key comments, and
/// connection hosts. Use [`argv_redactor`]
/// to also catch short bare `sk-…` tokens — needed when scrubbing subprocess
/// argv where a provider key sometimes ends up mis-configured.
///
/// Pattern compilation is process-cached. [`Regex`] is Arc-backed, so cloning
/// a redactor does not recompile.
#[derive(Clone)]
pub struct PatternRedactor {
    patterns: Vec<(Regex, &'static str)>,
    pattern_set: RegexSet,
}

impl PatternRedactor {
    /// Shared default for unknown-shape persisted text.
    ///
    /// Clones the process-cached HTTP redactor; patterns are compiled once.
    pub fn http_default() -> Self {
        default_pattern_redactor().clone()
    }

    fn compile_http_default() -> Self {
        let patterns = vec![
            (
                Regex::new(r#"(?i)"authorization"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""authorization":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)"x-api-key"\s*:\s*"(?:\\[^\r\n]|[^"\\\r\n])*""#)
                    .expect("valid regex"),
                r#""x-api-key":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)"api[_-]?key"\s*:\s*"(?:\\[^\r\n]|[^"\\\r\n])*""#)
                    .expect("valid regex"),
                r#""api_key":"[REDACTED_AUTH]""#,
            ),
            // Pretty-printed JSON API-key properties allow JSON whitespace
            // (including line breaks) around the colon. Keep this separate
            // from general assignments, whose horizontal-only separators
            // prevent an empty assignment from consuming the next line.
            (
                Regex::new(
                    r#"(?i)(^|[^\p{L}\p{N}_-])("(?:[a-z0-9]+[_-])*api[_-]?key"[ \t\r\n]*:[ \t\r\n]*")(?:\\[^\r\n]|[^"\\\r\n])+(\")"#,
                )
                .expect("valid regex"),
                "$1$2[REDACTED_AUTH]$3",
            ),
            (
                Regex::new(
                    r#"(^|[^\p{L}\p{N}_-])("(?:(?:[A-Za-z0-9_-]*?(?:[a-z0-9](?:Api|API)|[A-Z]Api|API)(?i:key)))"[ \t\r\n]*:[ \t\r\n]*")(?:\\[^\r\n]|[^"\\\r\n])+(\")"#,
                )
                .expect("valid regex"),
                "$1$2[REDACTED_AUTH]$3",
            ),
            // Same-line API-key assignments (`key=value` / `key: value`),
            // wherever they appear on the line, not only at line start.
            // The key must end in an `api key` segment at a separator,
            // camel-case or all-caps `API` boundary (mirroring the log
            // window classifier), so `capiKey` and `api_keyboard` stay
            // readable. An empty value has nothing to mask. Separator
            // whitespace is horizontal-only so an empty assignment never
            // consumes a value from the following line.
            (
                Regex::new(
                    r#"(?i)(^|[^\p{L}\p{N}_-])(((?:[a-z0-9]+[_-])*api[_-]?key)["'\)\]]?[ \t]*[:=][ \t]*")(?:\\[^\r\n]|[^"\\\r\n])+(\")"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${4}",
            ),
            (
                Regex::new(
                    r#"(?i)(^|[^\p{L}\p{N}_-])(((?:[a-z0-9]+[_-])*api[_-]?key)["'\)\]]?[ \t]*[:=][ \t]*')(?:\\[^\r\n]|[^'\\\r\n])+(')"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${4}",
            ),
            // Literal single-quoted values ending in one backslash: the
            // escape-aware entry above consumes the closing quote as an
            // escape, so this ordered fallback (no quote in the value body,
            // final backslash before the terminator) masks them instead.
            (
                Regex::new(
                    r#"(?i)(^|[^\p{L}\p{N}_-])(((?:[a-z0-9]+[_-])*api[_-]?key)["'\)\]]?[ \t]*[:=][ \t]*')(?:[^'\r\n]*\\)(')"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${4}",
            ),
            (
                Regex::new(
                    r#"(^|[^\p{L}\p{N}_-])((?:[A-Za-z0-9_-]*?(?:[a-z0-9](?:Api|API)|[A-Z]Api|API)(?i:key))["'\)\]]?[ \t]*[:=][ \t]*")(?:\\[^\r\n]|[^"\\\r\n])+(\")"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${3}",
            ),
            (
                Regex::new(
                    r#"(^|[^\p{L}\p{N}_-])((?:[A-Za-z0-9_-]*?(?:[a-z0-9](?:Api|API)|[A-Z]Api|API)(?i:key))["'\)\]]?[ \t]*[:=][ \t]*')(?:\\[^\r\n]|[^'\\\r\n])+(')"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${3}",
            ),
            // Camel-case counterpart of the literal trailing-backslash
            // fallback above.
            (
                Regex::new(
                    r#"(^|[^\p{L}\p{N}_-])((?:[A-Za-z0-9_-]*?(?:[a-z0-9](?:Api|API)|[A-Z]Api|API)(?i:key))["'\)\]]?[ \t]*[:=][ \t]*')(?:[^'\r\n]*\\)(')"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]${3}",
            ),
            (
                Regex::new(
                    r#"(?i)(^|[^\p{L}\p{N}_-])(((?:[a-z0-9]+[_-])*api[_-]?key)["'\)\]]?[ \t]*[:=][ \t]*)(?:\[REDACTED[A-Z_]+\]|[^\s"',;\]\)}]+)"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]",
            ),
            (
                Regex::new(
                    r#"(^|[^\p{L}\p{N}_-])((?:[A-Za-z0-9_-]*?(?:[a-z0-9](?:Api|API)|[A-Z]Api|API)(?i:key))["'\)\]]?[ \t]*[:=][ \t]*)(?:\[REDACTED[A-Z_]+\]|[^\s"',;\]\)}]+)"#,
                )
                .expect("valid regex"),
                "${1}${2}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r#"(?i)bearer\s+[A-Za-z0-9._\-+/=]+"#).expect("valid regex"),
                "Bearer [REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*authorization\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*x-api-key\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?im)^(\s*api[_-]?key\s*:\s*).+$").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            (
                Regex::new(r"(?i)([?&]key=)[^&\s]+").expect("valid regex"),
                "${1}[REDACTED_AUTH]",
            ),
            // `ssh-keygen -l` output: redact the fingerprint and the key comment,
            // while retaining key size and algorithm for diagnostic value.
            (
                Regex::new(
                    r"(?im)^(\s*\d{3,4}\s+)SHA256:[A-Za-z0-9+/]{43}\s+[^\r\n]+?\s+(\((?:RSA|DSA|ECDSA|ED25519)\))\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_FINGERPRINT] [REDACTED_SSH_KEY_COMMENT] ${2}",
            ),
            // OpenSSH verbose key-offer lines identify the local key through a
            // path or comment immediately before its algorithm and fingerprint.
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?(?:Offering public key|Will attempt key|Server accepts key):\s+)[^\r\n]+?\s+((?:RSA|DSA|ECDSA|ED25519)(?:-SK)?\s+)SHA256:[A-Za-z0-9+/]{43}([^\r\n]*)$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_KEY_COMMENT] ${2}[REDACTED_SSH_FINGERPRINT]${3}",
            ),
            // A serialized OpenSSH public key may carry an arbitrary comment
            // after its base64 material. Preserve the public key, redact only
            // the comment that commonly names a person, host, or key role.
            (
                Regex::new(
                    r"(?im)^(\s*(?:ssh-(?:rsa|dss|ed25519)|ecdsa-sha2-nistp(?:256|384|521)|sk-(?:ssh-ed25519|ecdsa-sha2-nistp256)@openssh\.com)\s+[A-Za-z0-9+/]{20,}={0,3})\s+[^\r\n]+$",
                )
                .expect("valid regex"),
                "${1} [REDACTED_SSH_KEY_COMMENT]",
            ),
            // Host redaction is deliberately limited to canonical OpenSSH
            // diagnostic sentences. A general hostname regex would erase
            // repository prose, model names, paths, and other useful records.
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?Connecting to\s+)\S+(?:\s+\[[^\]\r\n]+\])?(\s+port\s+\d+\.)\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(
                    r"(?im)^(\s*(?:debug\d+:\s*)?Authenticating to\s+)\S+(\s+as\s+[^\r\n]+)$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(
                    r"(?im)^(\s*Authenticated to\s+)\S+(?:\s+\([^\r\n)]+\))?(\.)\s*$",
                )
                .expect("valid regex"),
                "${1}[REDACTED_SSH_HOST]${2}",
            ),
            (
                Regex::new(r"SHA256:[A-Za-z0-9+/]{43}=").expect("valid regex"),
                "[REDACTED_SSH_FINGERPRINT]",
            ),
            (
                Regex::new(r"SHA256:[A-Za-z0-9+/]{43}\b").expect("valid regex"),
                "[REDACTED_SSH_FINGERPRINT]",
            ),
            (
                // Keep provider-key matching at a token boundary. Without the
                // prefix capture, `task-checkout-projections` is misread from
                // its trailing `sk-` as a provider key.
                Regex::new(r"(^|[^\p{L}\p{N}_-])sk-[A-Za-z0-9_\-]{20,}")
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"AIza[0-9A-Za-z_\-]{35}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"glpat-[A-Za-z0-9_\-]{20,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"github_pat_[A-Za-z0-9_]{22,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"gh[opsur]_[A-Za-z0-9]{36,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"AKIA[0-9A-Z]{16}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r#"(?i)("aws[_-]?secret[_-]?access[_-]?key"\s*:\s*")[^"]*""#)
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]\"",
            ),
            (
                Regex::new(r"(?i)\b(aws[_-]?secret[_-]?access[_-]?key\s*=\s*)[^&\s]+")
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"npm_[A-Za-z0-9]{36}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"([A-Za-z][A-Za-z0-9+.\-]*://[^/\s:@?#]+:)[^@\s/?#]+(@)")
                    .expect("valid regex"),
                "${1}[REDACTED_SECRET]${2}",
            ),
            (
                Regex::new(r"ghp_[A-Za-z0-9]{36}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            (
                Regex::new(r"xox[baprs]-[A-Za-z0-9\-]{10,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
            // Orbit lease listener bearer tokens (`orbit-auth`).
            (
                Regex::new(r"orbl_[A-Za-z0-9]{32,}").expect("valid regex"),
                "[REDACTED_SECRET]",
            ),
        ];
        let mut redactor = Self {
            patterns,
            pattern_set: RegexSet::empty(),
        };
        redactor.rebuild_pattern_set();
        redactor
    }

    fn compile_argv_secrets() -> Self {
        let mut me = default_pattern_redactor().clone();
        me.patterns.push((
            Regex::new(r"(^|[^\p{L}\p{N}_-])sk-[A-Za-z0-9_\-]+").expect("valid regex"),
            "${1}[REDACTED_API_KEY]",
        ));
        me.rebuild_pattern_set();
        me
    }

    fn rebuild_pattern_set(&mut self) {
        self.pattern_set = RegexSet::new(self.patterns.iter().map(|(pattern, _)| pattern.as_str()))
            .expect("valid redaction regex set");
    }

    /// Apply all patterns in order to `input`.
    pub fn apply_str(&self, input: &str) -> String {
        self.apply_str_cow(input).into_owned()
    }

    fn apply_str_cow<'a>(&self, input: &'a str) -> Cow<'a, str> {
        if !self.pattern_set.is_match(input) {
            return Cow::Borrowed(input);
        }

        self.apply_matching_str_cow(input)
    }

    fn apply_matching_str_cow<'a>(&self, input: &'a str) -> Cow<'a, str> {
        let mut out: Cow<'_, str> = Cow::Borrowed(input);
        for (pattern, replacement) in &self.patterns {
            match pattern.replace_all(&out, *replacement) {
                Cow::Borrowed(_) => {}
                Cow::Owned(new) => out = Cow::Owned(new),
            }
        }
        out
    }
}

impl Default for PatternRedactor {
    fn default() -> Self {
        Self::http_default()
    }
}

// ---------------------------------------------------------------------------
// Combined
// ---------------------------------------------------------------------------

/// Apply env-value and default pattern redaction in one pass. Use when the
/// input shape is unknown (knowledge records, log lines, aggregated errors).
pub fn redact_all(input: &str) -> String {
    if redaction_fast_miss(input).is_some() {
        return input.to_string();
    }
    redact_scanners(input)
}

/// Redact an owned string, reusing its allocation when it contains no secret.
pub fn redact_all_owned(input: String) -> String {
    if redaction_fast_miss(&input).is_some() {
        return input;
    }
    redact_owned_scanners(input)
}

/// [`redact_all_owned`] plus whether the returned text contains an HTML
/// metacharacter (`&`, `<`, `>`, `"`, `'`).
///
/// A fast miss reports the input exactly, so the flag lets a caller skip a
/// second scan before escaping. Text that still needs the scanners returns
/// `true`: the replacement may contain a metacharacter, and the caller checks.
pub fn redact_all_owned_flagging_html(input: String) -> (String, bool) {
    if let Some(needs_html_escape) = redaction_fast_miss(&input) {
        return (input, needs_html_escape);
    }
    (redact_owned_scanners(input), true)
}

fn redact_scanners(input: &str) -> String {
    let env_scrubbed = redact_sensitive_env_text_cow(input);
    let redactor = default_pattern_redactor();
    if redactor.pattern_set.is_match(env_scrubbed.as_ref()) {
        redactor
            .apply_matching_str_cow(env_scrubbed.as_ref())
            .into_owned()
    } else {
        env_scrubbed.into_owned()
    }
}

fn redact_owned_scanners(input: String) -> String {
    let env_scrubbed = redact_sensitive_env_text_cow(&input);
    let redactor = default_pattern_redactor();
    if redactor.pattern_set.is_match(env_scrubbed.as_ref()) {
        redactor
            .apply_matching_str_cow(env_scrubbed.as_ref())
            .into_owned()
    } else {
        match env_scrubbed {
            Cow::Borrowed(_) => input,
            Cow::Owned(redacted) => redacted,
        }
    }
}

/// ASCII fragments that the HTTP patterns in [`PatternRedactor::compile_http_default`]
/// must contain. Matching is impossible when every fragment has a byte absent
/// from the haystack's ASCII-case-folded alphabet. A fragment is only a
/// necessary condition: text that merely contains those bytes still goes
/// through the compiled regex. Non-ASCII haystacks skip this check, because
/// Unicode case-folding can match an ASCII pattern without those ASCII bytes.
const PATTERN_TRIGGER_BYTES: &[&[u8]] = &[
    b"authorization",
    b"apikey",
    b"bearer",
    b"key=",
    b"sha256:",
    b"sk-",
    b"aiza",
    b"glpat-",
    b"github_pat_",
    b"gho_",
    b"ghp_",
    b"ghs_",
    b"ghu_",
    b"ghr_",
    b"akia",
    b"awssecretaccesskey",
    b"npm_",
    b"://",
    b"xoxa-",
    b"xoxb-",
    b"xoxp-",
    b"xoxr-",
    b"xoxs-",
    b"orbl_",
    b"ssh-rsa",
    b"ssh-dss",
    b"ssh-ed25519",
    b"ecdsa-sha2-nistp",
    b"connecting to",
    b"authenticating to",
    b"authenticated to",
];

/// `Some(needs_html_escape)` when every env value and pattern misses `input`.
/// `None` when the scanners have to run. Non-ASCII text is `None`: Unicode
/// case-folding can match an ASCII pattern without the trigger's ASCII bytes.
/// HTML metacharacters are recorded from the same pass so a caller can escape
/// without scanning again.
fn redaction_fast_miss(input: &str) -> Option<bool> {
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return Some(false);
    }
    let mut exact = [0_u64; 4];
    let mut saw_uppercase = false;
    for &byte in bytes {
        if byte >= 128 {
            return None;
        }
        if byte.is_ascii_uppercase() {
            saw_uppercase = true;
        }
        exact[(byte >> 6) as usize] |= 1_u64 << (byte & 63);
    }
    let folded = if saw_uppercase {
        fold_ascii_uppercase(exact)
    } else {
        exact
    };
    if alphabet_contains_any(&folded, PATTERN_TRIGGER_BYTES)
        || super::env::sensitive_env_alphabet_possible(&exact)
    {
        return None;
    }
    Some(alphabet_needs_html_escape(&exact))
}

/// Copy `A`..=`Z` bits onto `a`..=`z`. Those letters occupy bits 1..=26 of
/// word 1, and the lowercase forms are 32 positions higher in the same word.
fn fold_ascii_uppercase(mut alphabet: [u64; 4]) -> [u64; 4] {
    const UPPER_BITS: u64 = (1_u64 << 27) - 2;
    alphabet[1] |= (alphabet[1] & UPPER_BITS) << 32;
    alphabet
}

fn alphabet_needs_html_escape(alphabet: &[u64; 4]) -> bool {
    [b'&', b'<', b'>', b'"', b'\'']
        .into_iter()
        .any(|byte| alphabet[(byte >> 6) as usize] & (1_u64 << (byte & 63)) != 0)
}

fn alphabet_contains_any(alphabet: &[u64; 4], needles: &[&[u8]]) -> bool {
    needles
        .iter()
        .any(|needle| alphabet_contains(alphabet, needle))
}

fn alphabet_contains(alphabet: &[u64; 4], needle: &[u8]) -> bool {
    needle
        .iter()
        .all(|&byte| alphabet[(byte >> 6) as usize] & (1_u64 << (byte & 63)) != 0)
}

/// Return true when `input` is exactly one high-confidence credential token.
///
/// Callers that persist free-text artifacts can reject these whole-token values
/// instead of merely masking them; embedded occurrences are still handled by
/// [`redact_all`].
pub fn is_high_confidence_single_token_credential(input: &str) -> bool {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() != 1 {
        return false;
    }
    high_confidence_single_token_patterns()
        .iter()
        .any(|pattern| pattern.is_match(trimmed))
}

/// Process-cached argv redactor (HTTP defaults plus the short `sk-…` pattern).
pub fn argv_redactor() -> &'static PatternRedactor {
    ARGV_PATTERN_REDACTOR.get_or_init(PatternRedactor::compile_argv_secrets)
}

pub(crate) fn default_pattern_redactor() -> &'static PatternRedactor {
    DEFAULT_PATTERN_REDACTOR.get_or_init(PatternRedactor::compile_http_default)
}

fn high_confidence_single_token_patterns() -> &'static [Regex] {
    HIGH_CONFIDENCE_SINGLE_TOKEN_PATTERNS
        .get_or_init(|| {
            vec![
                Regex::new(r"^sk-[A-Za-z0-9_\-]{20,}$").expect("valid regex"),
                Regex::new(r"^AIza[0-9A-Za-z_\-]{35}$").expect("valid regex"),
                Regex::new(r"^glpat-[A-Za-z0-9_\-]{20,}$").expect("valid regex"),
                Regex::new(r"^github_pat_[A-Za-z0-9_]{22,}$").expect("valid regex"),
                Regex::new(r"^gh[opsur]_[A-Za-z0-9]{36,}$").expect("valid regex"),
                Regex::new(r"^AKIA[0-9A-Z]{16}$").expect("valid regex"),
                Regex::new(r"(?i)^aws[_-]?secret[_-]?access[_-]?key=[^&\s]+$")
                    .expect("valid regex"),
                Regex::new(r"^npm_[A-Za-z0-9]{36}$").expect("valid regex"),
                Regex::new(
                    r"^[A-Za-z][A-Za-z0-9+.\-]*://[^/\s:@?#]+:[^@\s/?#]+@[^/\s?#]+(?:[/?#]\S*)?$",
                )
                .expect("valid regex"),
                Regex::new(r"^ghp_[A-Za-z0-9]{36}$").expect("valid regex"),
                Regex::new(r"^xox[baprs]-[A-Za-z0-9\-]{10,}$").expect("valid regex"),
                Regex::new(r"^orbl_[A-Za-z0-9]{32,}$").expect("valid regex"),
            ]
        })
        .as_slice()
}
