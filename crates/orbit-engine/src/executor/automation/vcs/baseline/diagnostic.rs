//! Generic comparison of complete diagnostics when no failure identities exist.

use std::path::Path;
use std::sync::OnceLock;

use orbit_common::security::release::sha256_hex;
use regex::Regex;

use super::super::base::claim::normalize_output;
use super::super::required::command::VALIDATION_PATH_LINE;

/// Hash complete, normalized diagnostic lines. Keep this separate from parsed
/// failures: a fingerprint can establish a hold, never test-only known red.
pub(in super::super) fn diagnostic_fingerprint(output: &str, root: &Path) -> Option<String> {
    static PATTERNS: OnceLock<Option<[Regex; 2]>> = OnceLock::new();
    let [temporary, elapsed] = PATTERNS
        .get_or_init(|| {
            Some([
                Regex::new(r#"(?:/private)?/(?:tmp|var/tmp)/[^\s"'`():,;]+|(?:/private)?/var/folders/[^/\s]+/[^/\s]+/T/[^\s"'`():,;]+"#).ok()?,
                Regex::new(r"\b(?:\d+h)?(?:\d+m)?\d+(?:\.\d+)?\s*(?:ns|us|µs|ms|s)\b").ok()?,
            ])
        })
        .as_ref()?;
    let canonical = std::fs::canonicalize(root)
        .ok()
        .map(|path| path.to_string_lossy().into_owned());
    let root = root.to_string_lossy();
    let mut lines = Vec::new();
    for line in output.lines() {
        let line = normalize_output(line);
        if line.starts_with(VALIDATION_PATH_LINE) || line.starts_with("PATH=") {
            continue;
        }
        let mut line = line.replace(root.as_ref(), "<workspace>");
        if let Some(canonical) = &canonical {
            line = line.replace(canonical, "<workspace>");
        }
        let line = temporary.replace_all(&line, "<temp>");
        let line = elapsed.replace_all(&line, "");
        let line = normalize_output(&line);
        if !line.is_empty() {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.sort_unstable();
    Some(format!(
        "diagnostic fingerprint sha256:{}",
        sha256_hex(lines.join("\n").as_bytes())
    ))
}
