//! Exact-consent recovery for interrupted plugin tool-name rewrites.
//!
//! New writes retain the original before publishing. Older writes have no
//! receipt: try the all-legacy form and at most 1,024 mixed forms, always
//! requiring both the recorded digest and an exact forward rewrite.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, sync_parent_dir};
use orbit_types::tool::pre_family_tool_name;

use super::ToolNameRewrite;
use super::auto_task_catalog::rewrite_yaml_keyed_list;
use super::plugin_manifest::plugin_manifest_tool_rewrite;

/// Bound compatibility work for writes made before originals were retained.
/// New writes recover from retained bytes regardless of the list's length.
const MAX_LEGACY_MIXED_ITEMS: usize = 10;

fn plugin_tool_original_path(global_root: &Path, digest: &str) -> Option<PathBuf> {
    // The database value is not a path component until it is a SHA-256 hex
    // digest. A malformed or absent digest never supplies consent evidence.
    (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
        global_root
            .join("state/plugin-tool-name-originals")
            .join(format!("{digest}.yaml"))
    })
}

/// Persist the exact, already digest-verified original before publishing its
/// rewrite. These content-addressed receipts stay available for retries.
pub(super) fn retain_plugin_tool_original(global_root: &Path, raw: &str) -> Result<(), OrbitError> {
    let digest = orbit_tools::plugin::manifest_digest(raw.as_bytes());
    let path = plugin_tool_original_path(global_root, &digest)
        .ok_or_else(|| OrbitError::Store("invalid plugin manifest digest".to_string()))?;
    let retained = (|| -> std::io::Result<()> {
        atomic_write_text(&path, raw)?;
        // The atomic write syncs the receipt's directory. Also persist that
        // directory's entry, and the state directory if this is its first
        // durable write, before publishing a manifest in another directory.
        sync_parent_dir(&fs::File::open(global_root.join("state"))?)?;
        sync_parent_dir(&fs::File::open(global_root)?)
    })();
    retained.map_err(|error| {
        OrbitError::io_with_context(
            &error,
            format!(
                "retain plugin manifest original {}: {error}",
                path.display()
            ),
        )
    })
}

/// Recognize only an exact forward rewrite of digest-verified consented
/// bytes. Prefer retained originals, which preserve mixed formats, duplicates
/// and layout without guessing. For pre-receipt writes, check the all-legacy
/// form and bounded mixed preimages; a larger ambiguous list needs re-consent.
pub(super) fn consented_plugin_tool_rewrite(
    global_root: &Path,
    path: &Path,
    name: &str,
    raw: &str,
    stored_digest: &str,
) -> Option<ToolNameRewrite> {
    let recognize = |original: &str| {
        if original == raw
            || orbit_tools::plugin::manifest_digest(original.as_bytes()) != stored_digest
        {
            return None;
        }
        let (healed, rewrite) = plugin_manifest_tool_rewrite(path, name, original)?;
        (healed == raw).then_some(rewrite)
    };
    if let Some(original) = plugin_tool_original_path(global_root, stored_digest)
        .and_then(|path| fs::read_to_string(path).ok())
        && let Some(rewrite) = recognize(&original)
    {
        return Some(rewrite);
    }

    let mut count = 0;
    let reversed = reverse_yaml_list_tool_names(raw, "orbit_tools", &mut || {
        count += 1;
        true
    });
    if let Some(rewrite) = recognize(&reversed) {
        return Some(rewrite);
    }
    if count > MAX_LEGACY_MIXED_ITEMS {
        return None;
    }
    // All-canonical is `raw` and all-legacy was tried above. The mask is
    // over item occurrences, so duplicates need not share their old spelling.
    for mask in 1..(1_usize << count).saturating_sub(1) {
        let mut index = 0;
        let candidate = reverse_yaml_list_tool_names(raw, "orbit_tools", &mut || {
            let reverse = mask & (1 << index) != 0;
            index += 1;
            reverse
        });
        if let Some(rewrite) = recognize(&candidate) {
            return Some(rewrite);
        }
    }
    None
}

/// Construct a possible old spelling, not an inverse: forward rewriting
/// leaves already-canonical actions alone. The caller chooses each occurrence
/// to reverse and must prove the result with the original digest.
fn reverse_yaml_list_tool_names(
    raw: &str,
    key: &str,
    reverse: &mut impl FnMut() -> bool,
) -> String {
    rewrite_yaml_keyed_list(raw, key, |line| reverse_line_tool_names(line, reverse))
}

/// Offer each known family-action list item for reversal, with the same item
/// boundaries as the forward rewrite. Prose and other tokens stay untouched.
fn reverse_line_tool_names(line: &str, reverse: &mut impl FnMut() -> bool) -> String {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let is_action_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut rewritten = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(is_name_char) {
        let (before, tail) = rest.split_at(start);
        let name_end = tail.find(|c: char| !is_name_char(c)).unwrap_or(tail.len());
        let mut end = name_end;
        if tail[name_end..].starts_with(':') {
            let action = &tail[name_end + 1..];
            let action_len = action
                .find(|c: char| !is_action_char(c))
                .unwrap_or(action.len());
            if action_len > 0 {
                end = name_end + 1 + action_len;
            }
        }
        let (token, after) = tail.split_at(end);
        rewritten.push_str(before);
        let opened = rewritten
            .trim_end()
            .trim_end_matches(['"', '\''])
            .trim_end()
            .ends_with(['-', '[', ',']);
        let closed = after
            .trim_start_matches(['"', '\''])
            .trim_start()
            .chars()
            .next()
            .is_none_or(|next| matches!(next, ',' | ']' | '#' | '\n' | '\r'));
        let old = pre_family_tool_name(token);
        if opened && closed && old != token && reverse() {
            rewritten.push_str(old);
        } else {
            rewritten.push_str(token);
        }
        rest = after;
    }
    rewritten.push_str(rest);
    rewritten
}
