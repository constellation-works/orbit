use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ScriptInputFile, ScriptNetwork, ScriptOutputFile};

/// The Linux script profile a `script_execution` record names.
pub const SCRIPT_PROFILE_LINUX: &str = "linux-bwrap-script-v1";

/// The macOS script profile a `script_execution` record names.
pub const SCRIPT_PROFILE_MACOS: &str = "macos-sandbox-script-v1";

/// The most stdout a script's JSON answer may be (§4).
pub const SCRIPT_STDOUT_CAP_BYTES: usize = 1024 * 1024;

/// Refusal codes a script step can stop with before anything is spawned.
/// Each is non-retryable: another attempt meets the same refusal.
pub mod script_refusal {
    /// No grant for this script name on this host (or its grant expired,
    /// or its row has no matching witness).
    pub const GRANT_MISSING: &str = "grant_missing";
    /// The grant was made at another digest than the bytes about to run.
    pub const GRANT_STALE: &str = "grant_stale";
    /// The manifest requests a capability the grant does not hold.
    pub const GRANT_INSUFFICIENT: &str = "grant_insufficient";
    /// A recorded interpreter path no longer resolves as it did at grant.
    pub const INTERPRETER_CHANGED: &str = "interpreter_changed";
    /// This host cannot apply the script profile (no Bubblewrap, no user
    /// namespaces, a nested sandbox, an unsupported platform or capability).
    pub const SANDBOX_UNAVAILABLE: &str = "script_sandbox_unavailable";
    /// A granted root, or the run's cwd tree, is missing, lies within a
    /// credential path, or fails the read (and, for a write root, write)
    /// probe the job's process makes before spawn.
    pub const ROOT_UNAVAILABLE: &str = "script_root_unavailable";
    /// A pull follower holds no grant made with `--follower`.
    pub const FOLLOWER_REFUSED: &str = "script_follower_refused";
    /// The manifest in the run's checkout no longer loads.
    pub const MANIFEST_INVALID: &str = "script_manifest_invalid";
    /// The step input fails the manifest's `input_schema_json`, or a
    /// templated argument does not render to one safe argv element.
    pub const INPUT_INVALID: &str = "script_input_invalid";
    /// A declared `files.inputs` entry is missing, resolves outside the cwd
    /// tree and the granted read roots, overlaps a masked tree or a
    /// credential path, or cannot be read to record its digest.
    pub const FILE_INPUT_UNAVAILABLE: &str = "script_file_input_unavailable";
}

/// Failure codes for a script that ran.
pub mod script_failure {
    /// Exit 0, but stdout is not JSON, is over the cap, or fails
    /// `output_schema_json`. Not retried.
    pub const OUTPUT_INVALID: &str = "script_output_invalid";
    /// The step's timeout expired; the sandbox was killed.
    pub const TIMEOUT: &str = "script_timeout";
    /// The scratch directory grew past the grant's cap; the sandbox was
    /// killed.
    pub const SCRATCH_CAP_EXCEEDED: &str = "script_scratch_cap_exceeded";
    /// Exit 0 with valid stdout, but `out/` holds more files or bytes than
    /// the manifest and grant allow. No file of the set is kept.
    pub const OUTPUTS_OVER_CAP: &str = "script_outputs_over_cap";
    /// Exit 0 with valid stdout, but `out/` holds a symlink, a directory,
    /// another non-regular entry or an unsafe name. No file is kept.
    pub const OUTPUTS_INVALID: &str = "script_outputs_invalid";
    /// The output set could not be stored, or a resumed run's recorded set
    /// no longer matches its producer record or bytes.
    pub const OUTPUTS_UNAVAILABLE: &str = "script_outputs_unavailable";
}

/// What one attempt of a script step recorded on its job run: identity,
/// what ran and what happened. Refusals record one too, with
/// `refusal_code` set and nothing spawned. Persisted on the run's v2 audit
/// trail as a `script.execution` event, so every field is additive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScriptExecutionRecord {
    pub script: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entry: String,
    /// Digest of the bytes snapshotted for this attempt; empty when the
    /// refusal came before the snapshot.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpreter_paths: Vec<PathBuf>,
    /// The argv the sandbox ran, redacted; empty for a refusal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// `worktree` (read-write) or `workspace_root` (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd_mode: Option<String>,
    /// The manifest's requested network until the grant is admitted, then
    /// the grant's: the network the script ran with.
    #[serde(default)]
    pub network: ScriptNetwork,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<PathBuf>,
    /// The granted `env_pass` names whose values reached the script. Names
    /// only: the values are redacted wherever the run's output is kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    /// Granted `env_pass` names this process did not hold, so the script ran
    /// with them unset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass_unset: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// The per-script systemd scope, when one was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_unit: Option<String>,
    /// `refused`, `exited`, `signaled`, `timed_out`, `scratch_cap_exceeded`
    /// or `spawn_failed`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_bytes: Option<u64>,
    /// Blob reference of the capped, redacted log (stdout and stderr
    /// interleaved).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_blob_ref: Option<String>,
    /// The refusal or failure detail, bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The accepted output set's identity, when the step kept files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_set_id: Option<String>,
    /// The kept output files with their sizes and SHA-256.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_files: Vec<ScriptOutputFile>,
    /// The declared input files as admitted before spawn, with their
    /// source paths and SHA-256.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_files: Vec<ScriptInputFile>,
}
