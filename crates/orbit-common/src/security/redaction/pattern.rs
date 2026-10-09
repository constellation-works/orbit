// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

use std::{borrow::Cow, sync::OnceLock};

use regex::Regex;

use super::env::redact_sensitive_env_text;

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
/// connection hosts. Use [`argv_redactor`] or [`PatternRedactor::with_argv_secrets`]
/// to also catch short bare `sk-…` tokens — needed when scrubbing subprocess
/// argv where a provider key sometimes ends up mis-configured.
///
/// Pattern compilation is process-cached. [`Regex`] is Arc-backed, so cloning
/// a redactor does not recompile.
#[derive(Clone)]
pub struct PatternRedactor {
    patterns: Vec<(Regex, &'static str)>,
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
                Regex::new(r#"(?i)"x-api-key"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""x-api-key":"[REDACTED_AUTH]""#,
            ),
            (
                Regex::new(r#"(?i)"api[_-]?key"\s*:\s*"[^"]*""#).expect("valid regex"),
                r#""api_key":"[REDACTED_AUTH]""#,
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
        ];
        Self { patterns }
    }

    /// HTTP defaults plus a bare `sk-…` token pattern suitable for scrubbing
    /// CLI argv where a provider key occasionally ends up as a flag value.
    ///
    /// Clones the process-cached argv redactor; patterns are compiled once.
    /// Prefer [`argv_redactor`] when an owned clone is not needed.
    pub fn with_argv_secrets() -> Self {
        argv_redactor().clone()
    }

    fn compile_argv_secrets() -> Self {
        let mut me = default_pattern_redactor().clone();
        me.patterns.push((
            Regex::new(r"(^|[^\p{L}\p{N}_-])sk-[A-Za-z0-9_\-]+").expect("valid regex"),
            "${1}[REDACTED_API_KEY]",
        ));
        me
    }

    pub fn empty() -> Self {
        Self { patterns: vec![] }
    }

    /// Apply all patterns in order to `input`.
    pub fn apply_str(&self, input: &str) -> String {
        let mut out: Cow<'_, str> = Cow::Borrowed(input);
        for (pattern, replacement) in &self.patterns {
            match pattern.replace_all(&out, *replacement) {
                Cow::Borrowed(_) => {}
                Cow::Owned(new) => out = Cow::Owned(new),
            }
        }
        out.into_owned()
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
    let env_scrubbed = redact_sensitive_env_text(input);
    default_pattern_redactor().apply_str(&env_scrubbed)
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
            ]
        })
        .as_slice()
}
