//! Text form of content digests.

/// Lowercase hex of `bytes`, two characters per byte.
///
/// Every SHA-256 identity Orbit persists (artifact digests, bundle and
/// generation-image digests, dedupe keys) is the lowercase hex of the raw
/// digest. `sha2` 0.11 outputs no longer implement `LowerHex`, so callers
/// pass the digest here instead of formatting it with `{:x}`; the text is the
/// same.
pub fn lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}
