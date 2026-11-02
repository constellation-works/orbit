//! Stateless atc1 cursor and canonical material fingerprints.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use orbit_types::tool::{AttentionCursorStatus, AttentionItem};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) type Seen = BTreeSet<[u8; 8]>;

pub(super) fn attention_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(values) => {
            let sorted: std::collections::BTreeMap<_, _> = values
                .iter()
                .map(|(k, v)| (k.clone(), canonical(v)))
                .collect();
            json!(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
pub(super) fn attention_fingerprint(item: &AttentionItem, state: Value) -> String {
    format!(
        "fp_{}",
        attention_digest(
            canonical(&json!({"id": item.id, "severity": item.severity, "state": state}))
                .to_string()
                .as_bytes()
        )
    )
}
pub(super) fn attention_seen_hash(item: &AttentionItem) -> [u8; 8] {
    let mut hash = Sha256::new();
    hash.update(item.id.as_bytes());
    hash.update([0]);
    hash.update(item.fingerprint.as_bytes());
    let digest = hash.finalize();
    let mut entry = [0; 8];
    entry.copy_from_slice(&digest[..8]);
    entry
}
pub(super) fn decode_attention_cursor(
    token: Option<&str>,
    now: u64,
    ttl_hours: u32,
) -> (AttentionCursorStatus, Seen) {
    use AttentionCursorStatus as S;
    let fallback = |status| (status, Seen::new());
    let Some(token) = token else {
        return fallback(S::None);
    };
    let Some(encoded) = token.strip_prefix("atc1.") else {
        return fallback(if token.starts_with("atc") && token.contains('.') {
            S::Unsupported
        } else {
            S::Invalid
        });
    };
    if encoded.len() > 10682 {
        return fallback(S::Invalid);
    }
    let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) else {
        return fallback(S::Invalid);
    };
    if bytes.len() < 11 {
        return fallback(S::Invalid);
    }
    if bytes[0] != 1 {
        return fallback(S::Unsupported);
    }
    let mut issued = [0; 8];
    issued.copy_from_slice(&bytes[1..9]);
    let issued = u64::from_be_bytes(issued);
    let count = u16::from_be_bytes([bytes[9], bytes[10]]) as usize;
    if count > 1000 || bytes.len() != 11 + count * 8 || issued > now.saturating_add(300) {
        return fallback(S::Invalid);
    }
    if now.saturating_sub(issued) > u64::from(ttl_hours) * 3600 {
        return fallback(S::Expired);
    }
    let seen = bytes[11..].as_chunks::<8>().0.iter().copied().collect();
    (S::Accepted, seen)
}
pub(super) fn encode_attention_cursor(now: u64, hashes: &[[u8; 8]]) -> String {
    let mut bytes = vec![1];
    bytes.extend(now.to_be_bytes());
    bytes.extend((hashes.len() as u16).to_be_bytes());
    for hash in hashes {
        bytes.extend(hash);
    }
    format!("atc1.{}", URL_SAFE_NO_PAD.encode(bytes))
}
