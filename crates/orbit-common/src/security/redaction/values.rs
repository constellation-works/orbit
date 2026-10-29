//! Explicit secret-value redaction at external data boundaries.

use super::{PatternRedactor, is_sensitive_env_name};
use serde_json::Value;

/// Scrub supplied secret values and credential shapes without reading ambient
/// environment data. Includes object keys, JSON/Debug encodings embedded in
/// strings and numeric values equal to a secret. Sensitive named properties
/// are masked in full. Non-string types otherwise retain their shape.
///
/// Callers must bound input size/depth first and reject a result that no longer
/// satisfies their typed contract; a secret-bearing identifier must not become
/// a new accepted identifier after redaction.
pub fn redact_secret_values_json(mut value: Value, secrets: &[String]) -> Value {
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
    let redactor = PatternRedactor::http_default();
    redact_values_recursive(&mut value, &variants, &redactor);
    value
}

fn redact_values_text(text: &str, variants: &[String], redactor: &PatternRedactor) -> String {
    let mut text = text.to_string();
    for secret in variants {
        text = text.replace(secret, "[REDACTED]");
    }
    redactor.apply_str(&text)
}

fn redact_values_recursive(value: &mut Value, variants: &[String], redactor: &PatternRedactor) {
    match value {
        Value::String(s) => *s = redact_values_text(s, variants, redactor),
        Value::Array(a) => {
            for v in a {
                redact_values_recursive(v, variants, redactor);
            }
        }
        Value::Object(o) => {
            let mut clean = serde_json::Map::new();
            for (key, mut v) in std::mem::take(o) {
                redact_values_recursive(&mut v, variants, redactor);
                let key = redact_values_text(&key, variants, redactor);
                if is_sensitive_env_name(&key) {
                    v = Value::String("[REDACTED]".into());
                }
                clean.insert(key, v);
            }
            *o = clean;
        }
        Value::Number(n) if variants.iter().any(|secret| secret == &n.to_string()) => {
            *value = Value::String("[REDACTED]".into())
        }
        _ => {}
    }
}
