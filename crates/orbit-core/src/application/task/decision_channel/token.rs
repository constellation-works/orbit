use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::DecisionRequest;
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;

const TOKEN_LEN: usize = 26;

pub(super) fn fresh_key_hex() -> Result<String, OrbitError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("draw decision channel key: {error}")))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn key_bytes(key_hex: &str) -> Result<Vec<u8>, OrbitError> {
    if !key_hex.len().is_multiple_of(2) || !key_hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(OrbitError::Execution(
            "decision channel key is not hex; rotate the channel key".into(),
        ));
    }
    (0..key_hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&key_hex[index..index + 2], 16)
                .map_err(|error| OrbitError::Execution(format!("decision channel key: {error}")))
        })
        .collect()
}

fn token_message(epoch: u64, request: &DecisionRequest) -> Vec<u8> {
    format!(
        "{}|{}|{}|{}",
        request.id,
        request.task_id,
        epoch,
        request.expires_at.timestamp()
    )
    .into_bytes()
}

fn mint_token(key_hex: &str, epoch: u64, request: &DecisionRequest) -> Result<String, OrbitError> {
    let mac = hmac_sha256(&key_bytes(key_hex)?, &token_message(epoch, request));
    Ok(base32(&mac).chars().take(TOKEN_LEN).collect())
}

pub(super) fn token_matches(
    presented: &str,
    key_hex: &str,
    epoch: u64,
    request: &DecisionRequest,
) -> Result<bool, OrbitError> {
    let expected = mint_token(key_hex, epoch, request)?;
    Ok(constant_time_eq(presented.as_bytes(), expected.as_bytes()))
}

pub(super) fn token_well_formed(token: &str) -> bool {
    token.len() == TOKEN_LEN && token.chars().all(|ch| matches!(ch, 'A'..='Z' | '2'..='7'))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// HMAC-SHA256 (RFC 2104) over the workspace's `sha2`, so the token
/// contract needs no further dependency.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(key_block.iter().map(|byte| byte ^ 0x36).collect::<Vec<_>>());
    inner.update(message);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(key_block.iter().map(|byte| byte ^ 0x5c).collect::<Vec<_>>());
    outer.update(inner_hash);
    outer.finalize().into()
}

/// RFC 4648 base32 without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

impl OrbitRuntime {
    /// Mint the token for one request under a channel's current key and
    /// epoch. Core's half of the token contract: a transport holding the
    /// mirrored `channel_key` secret mints the same value itself; this
    /// method serves the operator testing a channel and tests. It is not
    /// exposed as a tool — a token must never be listable by the agents
    /// who may read the request it answers.
    pub fn mint_decision_token(
        &self,
        channel: &str,
        request_id: &str,
    ) -> Result<String, OrbitError> {
        let registration = self.active_registration(channel)?;
        let request = self.get_decision_request(request_id)?.ok_or_else(|| {
            OrbitError::decision_refused(
                DecisionRefusalCode::RequestNotFound,
                format!("no decision request `{request_id}` in this workspace"),
            )
        })?;
        mint_token(&registration.key_hex, registration.epoch, &request)
    }
}
