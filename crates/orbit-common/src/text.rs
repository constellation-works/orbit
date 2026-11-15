//! Shared text mechanisms: UTF-8-safe byte-index helpers for bounding
//! arbitrary text, and Unicode-scalar edit distance for typo suggestions.
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

/// Whether `haystack.to_lowercase()` contains `lowered_needle`, which the
/// caller has already lowercased once, without allocating for ASCII text.
///
/// Matches the `haystack.to_lowercase().contains(lowered_needle)` idiom
/// exactly, including its Unicode behavior (`'\u{212A}'` KELVIN SIGN lowercases
/// to `'k'`): when both sides are ASCII the comparison is a windowed
/// byte-wise ASCII case-insensitive one; any non-ASCII haystack falls back to
/// the allocating lowercase, and an ASCII haystack cannot contain a
/// non-ASCII needle.
pub fn contains_lowercased(haystack: &str, lowered_needle: &str) -> bool {
    if lowered_needle.is_empty() {
        return true;
    }
    if !haystack.is_ascii() {
        return haystack.to_lowercase().contains(lowered_needle);
    }
    if !lowered_needle.is_ascii() {
        return false;
    }
    haystack
        .as_bytes()
        .windows(lowered_needle.len())
        .any(|window| window.eq_ignore_ascii_case(lowered_needle.as_bytes()))
}

/// Levenshtein distance between `left` and `right` over Unicode scalars
/// (`char`s, not bytes or grapheme clusters): the minimum number of scalar
/// insertions, deletions and substitutions, each costing 1, that turns one
/// into the other. No normalization is applied.
pub fn unicode_edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (index, left) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = index + 1;
        for (column, right) in right.iter().enumerate() {
            let previous = row[column + 1];
            row[column + 1] = (diagonal + usize::from(left != *right))
                .min(previous + 1)
                .min(row[column] + 1);
            diagonal = previous;
        }
    }
    row[right.len()]
}
