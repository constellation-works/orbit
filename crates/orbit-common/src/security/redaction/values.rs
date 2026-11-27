//! Explicit secret-value redaction at external data boundaries.

use super::PatternRedactor;

/// Scrub supplied secret values from captured text: every secret value, and its
/// JSON-string and Debug encodings, becomes `[REDACTED]`, then credential shapes
/// are scrubbed. For captured process output (a log, a stdout document) that may
/// echo values the caller handed the process.
pub fn redact_secret_values_text(text: &str, secrets: &[String]) -> String {
    redact_values_text(
        text,
        &secret_variants(secrets),
        &PatternRedactor::http_default(),
    )
}

/// Each non-empty secret with its JSON-string and Debug encodings, longest
/// first so a longer variant is replaced before a prefix of it.
fn secret_variants(secrets: &[String]) -> Vec<String> {
    let mut variants = Vec::new();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        variants.push(secret.clone());
        if let Ok(encoded) = serde_json::to_string(secret) {
            variants.push(encoded[1..encoded.len() - 1].to_string());
        }
        let debug = format!("{secret:?}");
        variants.push(debug[1..debug.len() - 1].to_string());
    }
    variants.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));
    variants.dedup();
    variants
}

fn redact_values_text(text: &str, variants: &[String], redactor: &PatternRedactor) -> String {
    let mut text = text.to_string();
    for secret in variants {
        text = text.replace(secret, "[REDACTED]");
    }
    redactor.apply_str(&text)
}
