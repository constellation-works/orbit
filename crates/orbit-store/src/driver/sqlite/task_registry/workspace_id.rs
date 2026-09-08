use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::Connection;

use super::queries::{workspace_by_id, workspace_checkout_by_id};
use crate::fs::path_safety::normalize_path;
pub(super) use crate::fs::path_safety::validate_workspace_id;

/// Allocate an unused logical workspace id for `slug` + `path`.
///
/// A candidate counts as taken when *either* registry table already claims it:
/// the logical `workspace_bindings` row or the machine-local
/// `workspace_checkout_bindings` row. Checking only the logical table let the
/// allocator hand back an id whose checkout row already existed, which then
/// failed the caller's checkout-collision check instead of retrying with the
/// next attempt (ORB-10507).
pub(super) fn next_workspace_id_candidate(
    conn: &Connection,
    slug: &str,
    path: &Path,
) -> Result<String, OrbitError> {
    for attempt in 0..1000 {
        let candidate = workspace_id_candidate(slug, path, attempt);
        if workspace_by_id(conn, &candidate)?.is_none()
            && workspace_checkout_by_id(conn, &candidate)?.is_none()
        {
            return Ok(candidate);
        }
    }
    Err(OrbitError::Store(format!(
        "could not allocate workspace id for slug '{slug}'"
    )))
}

pub(super) fn workspace_id_candidate(slug: &str, path: &Path, attempt: u32) -> String {
    let input = format!("{}:{}:{attempt}", slug, normalize_path(path).display());
    let hash = blake3::hash(input.as_bytes()).to_hex();
    format!("{slug}-{}", &hash[..6])
}

pub(super) fn sanitize_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "workspace".to_string()
    } else {
        out
    }
}
