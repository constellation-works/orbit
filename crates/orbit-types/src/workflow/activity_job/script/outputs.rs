use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::lower_hex;

/// Caps on the files a script may return from its `out/` directory.
///
/// Setting either cap declares that the script returns files: after a
/// successful exit with valid stdout, Orbit keeps every top-level regular
/// file of `out/` as one run-owned output set. A manifest that sets
/// neither declares no outputs, and `out/` is discarded with the scratch
/// directory. An unset cap takes the grant's.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptFileOutputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,
}

impl ScriptFileOutputs {
    /// Whether the manifest declares output files at all.
    pub fn declared(&self) -> bool {
        self.max_files.is_some() || self.max_file_bytes.is_some()
    }
}

/// The most output files any grant may allow one step.
pub const SCRIPT_OUTPUT_HARD_MAX_FILES: u32 = 64;
/// The largest one output file any grant may allow.
pub const SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// The most bytes one step's whole output set may hold, whatever the
/// per-file caps multiply to.
pub const SCRIPT_OUTPUT_HARD_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// The longest output file name.
pub const SCRIPT_OUTPUT_MAX_NAME_BYTES: usize = 128;
/// Domain separator for an output set's identity.
pub const SCRIPT_OUTPUT_SET_DOMAIN: &str = "orbit.script.outputs.v1";

/// The aggregate byte bound of one output set under per-file caps: their
/// product, never above [`SCRIPT_OUTPUT_HARD_MAX_TOTAL_BYTES`].
pub fn script_output_total_cap(max_files: u32, max_file_bytes: u64) -> u64 {
    u64::from(max_files)
        .saturating_mul(max_file_bytes)
        .min(SCRIPT_OUTPUT_HARD_MAX_TOTAL_BYTES)
}

/// Whether `name` may name an output file: 1–128 bytes of ASCII letters,
/// digits, `.`, `_` and `-`, not starting with `.`.
pub fn is_valid_script_output_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= SCRIPT_OUTPUT_MAX_NAME_BYTES
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The media type an output file is described with, from its extension.
/// Unknown extensions are `application/octet-stream`.
pub fn script_output_media_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// One file of a script step's output set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptOutputFile {
    /// The file's name in `out/`, a safe top-level name.
    pub name: String,
    pub bytes: u64,
    pub media_type: String,
    /// SHA-256 of the exact bytes, lowercase hex.
    pub sha256: String,
    /// Where the durable copy lives on the producing host. A reference,
    /// not authority: readers check it against the producer record.
    pub path: String,
}

/// The durable descriptor of one script step's output set: which
/// workspace, run and step produced which files. Written by the host into
/// the producer record beside the files and carried by the run's pipeline
/// state; a descriptor is honoured only while the producer record and the
/// file bytes still match it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptOutputSet {
    pub workspace_id: String,
    pub run_id: String,
    pub step_id: String,
    /// [`script_output_set_id`] of the fields above and the files.
    pub set_id: String,
    pub files: Vec<ScriptOutputFile>,
}

impl ScriptOutputSet {
    /// The total bytes of the set.
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }

    /// Whether `set_id` is the identity of the other fields.
    pub fn identity_matches(&self) -> bool {
        self.set_id
            == script_output_set_id(&self.workspace_id, &self.run_id, &self.step_id, &self.files)
    }
}

/// The identity of one output set: a digest over its producer and every
/// file's name, size and SHA-256 (not its path), in order.
pub fn script_output_set_id(
    workspace_id: &str,
    run_id: &str,
    step_id: &str,
    files: &[ScriptOutputFile],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SCRIPT_OUTPUT_SET_DOMAIN.as_bytes());
    for part in [workspace_id, run_id, step_id] {
        hasher.update(b"\n");
        hasher.update(part.as_bytes());
    }
    for file in files {
        hasher.update(b"\n");
        hasher.update(file.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(file.bytes.to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(file.sha256.as_bytes());
    }
    let hex = lower_hex(hasher.finalize());
    format!("so-{}", &hex[..32])
}
