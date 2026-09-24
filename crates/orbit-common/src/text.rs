//! UTF-8-safe byte-index helpers for bounding arbitrary text.
//!
//! They stand in for `str::floor_char_boundary` / `str::ceil_char_boundary`,
//! which are newer than the workspace MSRV. Callers bound subprocess output,
//! logs, and diagnostics, where slicing mid-codepoint would panic on exactly
//! the inputs the bound exists to handle.

/// Largest index at or below `index` on a `char` boundary of `text`, clamped
/// to `text.len()`.
pub fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut end = index;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Smallest index at or above `index` on a `char` boundary of `text`, clamped
/// to `text.len()`.
pub fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut start = index.min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

#[cfg(test)]
#[path = "tests/text.rs"]
mod tests;
