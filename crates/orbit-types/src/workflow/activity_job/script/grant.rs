use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ScriptHostRole, ScriptNetwork};
use crate::identity::lower_hex;

/// Domain separator for the digest a script grant binds. Bumping it
/// invalidates every grant, which fails closed.
pub const SCRIPT_GRANT_DIGEST_DOMAIN: &str = "orbit.script.grant.v1";

/// Resource ceilings one grant carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptGrantLimits {
    pub timeout_max_seconds: u64,
    pub scratch_max_bytes: u64,
    pub output_max_files: u32,
    pub output_max_file_bytes: u64,
}

impl Default for ScriptGrantLimits {
    fn default() -> Self {
        Self {
            timeout_max_seconds: 3600,
            scratch_max_bytes: 1024 * 1024 * 1024,
            output_max_files: 8,
            output_max_file_bytes: 1024 * 1024,
        }
    }
}

/// One operator grant: host-local consent to one workspace script name at
/// one content digest, with an explicit capability set. Persisted in the
/// `script_grant` table and witnessed outside the database; the loader
/// refuses a row whose witness does not match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScriptGrant {
    pub grant_id: String,
    pub workspace_id: String,
    pub name: String,
    /// Combined digest over manifest and entry bytes
    /// ([`script_content_digest`]).
    pub digest: String,
    /// SHA-256 of the manifest bytes alone, so staleness can say which
    /// half moved.
    pub manifest_digest: String,
    /// SHA-256 of the entry bytes alone.
    pub entry_digest: String,
    /// Canonical interpreter paths resolved at grant time. Empty for a
    /// native executable entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpreter_paths: Vec<PathBuf>,
    #[serde(default)]
    pub network: ScriptNetwork,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    #[serde(default)]
    pub limits: ScriptGrantLimits,
    #[serde(default)]
    pub host_role: ScriptHostRole,
    pub grantor: String,
    pub granted_at: String,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// SHA-256 of one byte string, lowercase hex.
pub fn script_sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    lower_hex(hasher.finalize())
}

/// The digest a grant binds: `sha256("orbit.script.grant.v1\n" ‖ manifest
/// bytes ‖ "\n" ‖ entry bytes)`. Any edit to the manifest or the entry
/// file changes it, which is what makes an edit void the grant.
pub fn script_content_digest(manifest_bytes: &[u8], entry_bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SCRIPT_GRANT_DIGEST_DOMAIN.as_bytes());
    hasher.update(b"\n");
    hasher.update(manifest_bytes);
    hasher.update(b"\n");
    hasher.update(entry_bytes);
    lower_hex(hasher.finalize())
}

/// The stable `sg-<hash>` identifier audit cites for one grant.
pub fn script_grant_id(workspace_id: &str, name: &str, digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(SCRIPT_GRANT_DIGEST_DOMAIN.as_bytes());
    hasher.update(b"\n");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(name.as_bytes());
    hasher.update(b"\n");
    hasher.update(digest.as_bytes());
    let hex = lower_hex(hasher.finalize());
    format!("sg-{}", &hex[..16])
}
