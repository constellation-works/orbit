//! The payload of a `script_grant` decision request (script steps §2 route
//! 3, phase 4): one workspace script at one digest with one requested
//! capability set, what it is refused against, and the diff from the last
//! granted bytes. Raising one never grants; only a human answer does.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ScriptGrant, ScriptHostRole, ScriptNetwork, ScriptSpec};
use crate::identity::lower_hex;

/// Tag on the `someday` holder task a workspace script's grant requests
/// live on. Agents skip `someday` tasks, so the holder is never dispatched.
pub const SCRIPT_GRANT_TASK_TAG: &str = "script-grant";

/// Most bytes of diff text a request carries; the rest is cut with a marker.
pub const SCRIPT_GRANT_DIFF_MAX_BYTES: usize = 32 * 1024;

/// Most lines on either side a diff aligns line by line. A larger file is
/// reported as replaced whole.
const SCRIPT_GRANT_DIFF_MAX_LINES: usize = 1000;

/// The capability set a grant request asks for, or a grant holds, in one
/// comparable shape. `None` in an output cap means the default ceiling.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptGrantCapabilities {
    #[serde(default)]
    pub network: ScriptNetwork,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    /// Requested step timeout (a request), or the grant's ceiling (a grant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_file_bytes: Option<u64>,
    #[serde(default)]
    pub host_role: ScriptHostRole,
}

impl ScriptGrantCapabilities {
    /// What a manifest asks for, on a host of `host_role`.
    pub fn requested_by(spec: &ScriptSpec, host_role: ScriptHostRole) -> Self {
        let requests = &spec.requests;
        Self {
            network: requests.network,
            read_roots: requests.read_roots.clone(),
            write_roots: requests.write_roots.clone(),
            env_pass: requests.env_pass.clone(),
            timeout_seconds: requests.timeout_seconds,
            output_max_files: spec.files.outputs.max_files,
            output_max_file_bytes: spec.files.outputs.max_file_bytes,
            host_role,
        }
    }

    /// What an existing grant holds, in the request's shape.
    pub fn of_grant(grant: &ScriptGrant) -> Self {
        Self {
            network: grant.network,
            read_roots: grant.read_roots.clone(),
            write_roots: grant.write_roots.clone(),
            env_pass: grant.env_pass.clone(),
            timeout_seconds: Some(grant.limits.timeout_max_seconds),
            output_max_files: Some(grant.limits.output_max_files),
            output_max_file_bytes: Some(grant.limits.output_max_file_bytes),
            host_role: grant.host_role,
        }
    }

    /// A stable key for this set: requests for the same digest and the same
    /// key are one request.
    pub fn key(&self) -> String {
        let canonical = serde_json::to_string(self).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(b"orbit.script.grant.capabilities.v1\n");
        hasher.update(canonical.as_bytes());
        lower_hex(hasher.finalize())[..16].to_string()
    }
}

/// What a `script_grant` request asks the human to consent to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptGrantRequest {
    pub workspace_id: String,
    pub name: String,
    /// The digest the answer grants, and only it.
    pub digest: String,
    pub manifest_digest: String,
    pub entry_digest: String,
    /// The workspace-relative entry path.
    pub entry: String,
    /// The refusal that raised the request (`grant_missing`, `grant_stale`
    /// or `grant_insufficient`).
    pub refusal: String,
    /// The refusal's message, naming what the grant lacks.
    pub refusal_message: String,
    /// The digest of the grant the script was refused against, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_grant_id: Option<String>,
    /// The capability set the answer grants.
    pub requested: ScriptGrantCapabilities,
    /// What the current grant holds, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted: Option<ScriptGrantCapabilities>,
    /// Unified-style line diff of the manifest and entry against the last
    /// granted bytes; every line is added when nothing was granted before.
    pub diff: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub diff_truncated: bool,
    /// The run and step whose refusal raised it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
}

/// A line diff of one file, `-`/`+`/` ` prefixed, with a `--- a`/`+++ b`
/// header and only changed lines plus up to two lines of context. Empty when
/// both sides are equal.
pub fn script_file_diff(label: &str, before: Option<&str>, after: &str) -> String {
    if before == Some(after) {
        return String::new();
    }
    let old: Vec<&str> = before
        .map(|text| text.lines().collect())
        .unwrap_or_default();
    let new: Vec<&str> = after.lines().collect();
    let mut out = format!(
        "--- {}\n+++ {label}\n",
        if before.is_some() {
            format!("granted/{label}")
        } else {
            "/dev/null".to_string()
        }
    );
    if old.len() > SCRIPT_GRANT_DIFF_MAX_LINES || new.len() > SCRIPT_GRANT_DIFF_MAX_LINES {
        for line in &old {
            out.push_str(&format!("-{line}\n"));
        }
        for line in &new {
            out.push_str(&format!("+{line}\n"));
        }
        return out;
    }
    // Longest common subsequence table, then a walk that emits the edit.
    let (n, m) = (old.len(), new.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            ops.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            // Removals first, as a unified diff reads.
            ops.push(('-', old[i]));
            i += 1;
        } else {
            ops.push(('+', new[j]));
            j += 1;
        }
    }
    const CONTEXT: usize = 2;
    let changed: Vec<bool> = ops.iter().map(|(kind, _)| *kind != ' ').collect();
    let near_change = |index: usize| {
        let start = index.saturating_sub(CONTEXT);
        let end = (index + CONTEXT + 1).min(ops.len());
        changed[start..end].iter().any(|changed| *changed)
    };
    let mut skipped = false;
    for (index, (kind, line)) in ops.iter().enumerate() {
        if near_change(index) {
            if skipped {
                out.push_str("@@\n");
                skipped = false;
            }
            out.push(*kind);
            out.push_str(line);
            out.push('\n');
        } else {
            skipped = true;
        }
    }
    out
}

/// The request diff for both files: the manifest, then the entry, cut at
/// [`SCRIPT_GRANT_DIFF_MAX_BYTES`]. Returns the text and whether it was cut.
pub fn script_grant_diff(
    manifest_label: &str,
    granted_manifest: Option<&str>,
    manifest: &str,
    entry_label: &str,
    granted_entry: Option<&str>,
    entry: &str,
) -> (String, bool) {
    let mut diff = script_file_diff(manifest_label, granted_manifest, manifest);
    diff.push_str(&script_file_diff(entry_label, granted_entry, entry));
    if diff.len() <= SCRIPT_GRANT_DIFF_MAX_BYTES {
        return (diff, false);
    }
    let mut cut = SCRIPT_GRANT_DIFF_MAX_BYTES;
    while !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    diff.truncate(cut);
    diff.push_str("\n… diff truncated; run `orbit script show` on the host for the files\n");
    (diff, true)
}
