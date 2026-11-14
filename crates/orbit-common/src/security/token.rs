//! Shared primitives for minting and comparing opaque tokens.

use crate::OrbitError;

/// Draw `byte_len` bytes from the OS random source and encode them as
/// lowercase hex, two characters per byte.
pub fn random_hex(byte_len: usize) -> Result<String, OrbitError> {
    let mut bytes = vec![0u8; byte_len];
    getrandom::fill(&mut bytes)
        .map_err(|error| OrbitError::Execution(format!("OS random source unavailable: {error}")))?;
    Ok(orbit_types::identity::lower_hex(bytes))
}

/// Compare equal-length bytes in time independent of the position of the
/// first difference. Different lengths are rejected immediately.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    std::hint::black_box(difference) == 0
}
