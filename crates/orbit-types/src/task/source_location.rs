//! Bare repository-relative `path:line` evidence syntax, without source lookup.

use std::str::FromStr;

/// Whether a source citation admits empty slash-separated path components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLocationEmptyComponents {
    /// Admit repeated or trailing slashes (but never an empty or absolute path).
    Allow,
    /// Refuse repeated or trailing slashes.
    Reject,
}

/// Syntax failure in a bare source citation, in validation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLocationParseError {
    /// No final colon separates the path and line.
    MissingSeparator,
    /// The line does not parse in the selected numeric type or is not positive.
    InvalidLine,
    /// The path is empty, absolute, parent-traversing, or violates component policy.
    InvalidPath,
}

/// Parse a bare `path:line`, trimming only the complete location and splitting
/// at its final colon. `Line` selects the caller's numeric bounds (for example,
/// `u32` or `usize`); its default value must represent zero.
///
/// Checks run in separator, positive-line, then relative-path order. The path
/// is borrowed unchanged: dot components, Unicode, colons and per-piece
/// whitespace are not normalized. This performs no I/O or image-region parsing.
pub fn parse_source_location<Line: FromStr + Default + PartialOrd>(
    location: &str,
    empty_components: SourceLocationEmptyComponents,
) -> Result<(&str, Line), SourceLocationParseError> {
    let (path, line) = location
        .trim()
        .rsplit_once(':')
        .ok_or(SourceLocationParseError::MissingSeparator)?;
    let line = line
        .parse::<Line>()
        .ok()
        .filter(|line| *line > Line::default())
        .ok_or(SourceLocationParseError::InvalidLine)?;
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|part| {
            part == ".."
                || (part.is_empty() && empty_components == SourceLocationEmptyComponents::Reject)
        })
    {
        return Err(SourceLocationParseError::InvalidPath);
    }
    Ok((path, line))
}
