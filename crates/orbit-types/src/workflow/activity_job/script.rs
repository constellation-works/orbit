//! Script step manifests and operator grants (script steps phase 1).
//!
//! A job step names a script as `target: script:<name>`. The name resolves
//! to a manifest at `.orbit/resources/scripts/<name>.yaml` in the workspace
//! whose `entry` is the program file Orbit will run in a later phase. This
//! module holds the manifest shape, the pure validation rules that do not
//! touch the filesystem (interpreter forms, inline-code flags, argument
//! templates), the content digest a grant binds, and the host-local grant
//! record. Filesystem resolution of `entry` lives with the catalog loader
//! in `orbit-engine`; nothing here executes anything.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::identity::lower_hex;

/// `script:<name>` prefix for the `target:` field on a [`super::TargetRef`].
pub const SCRIPT_REF_PREFIX: &str = "script:";

/// The only `schemaVersion` a script manifest may declare.
pub const SCRIPT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Domain separator for the digest a script grant binds. Bumping it
/// invalidates every grant, which fails closed.
pub const SCRIPT_GRANT_DIGEST_DOMAIN: &str = "orbit.script.grant.v1";

/// A script manifest's `spec:` body.
///
/// ```yaml
/// schemaVersion: 1
/// kind: Script
/// metadata:
///   name: package
/// spec:
///   description: Package the built site.
///   entry: tools/package.sh
///   interpreter: [bash]
///   args: ["--out", "out/", "--label={{ input.label }}"]
///   requests: {network: none}
/// ```
///
/// There is deliberately no `command`, `script` or `run` text field: the
/// manifest names code in a file, never inline code, and unknown fields are
/// refused so such a field cannot be smuggled in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptSpec {
    #[serde(default)]
    pub description: String,
    /// Workspace-relative path of the program file. Resolved physically at
    /// load; it must stay beneath the workspace root.
    pub entry: String,
    /// Optional launcher argv (`[bash]`, `[uv, run, --offline, --script]`).
    /// Empty means the entry's `#!` line decides, or the entry is a native
    /// executable. Elements are literal: no templates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpreter: Vec<String>,
    /// Arguments handed to the entry after the launcher. Each element is
    /// exactly one argv element; `{{ … }}` templates render against the
    /// step input at run time and are never split or globbed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default)]
    pub input_schema_json: Value,
    #[serde(default)]
    pub output_schema_json: Value,
    #[serde(default)]
    pub files: ScriptFiles,
    /// Capability requests. These ask; the operator grant decides.
    #[serde(default)]
    pub requests: ScriptRequests,
}

/// Declared file inputs and output caps for a script step.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptFiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ScriptFileInput>,
    #[serde(default)]
    pub outputs: ScriptFileOutputs,
}

/// One declared file input, mounted read-only at `in/<as>` at run time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptFileInput {
    pub from: String,
    #[serde(rename = "as")]
    pub as_: String,
}

/// Caps on the files a script may return from its `out/` directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptFileOutputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_bytes: Option<u64>,
}

/// What a script asks its grant for. A request beyond the grant refuses the
/// step at run time; in phase 1 the requests are recorded and shown at
/// grant time only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptRequests {
    #[serde(default)]
    pub network: ScriptNetwork,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_pass: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Network reach a script grant may carry.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScriptNetwork {
    #[default]
    None,
    Https,
    Any,
}

impl ScriptNetwork {
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptNetwork::None => "none",
            ScriptNetwork::Https => "https",
            ScriptNetwork::Any => "any",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(ScriptNetwork::None),
            "https" => Some(ScriptNetwork::Https),
            "any" => Some(ScriptNetwork::Any),
            _ => None,
        }
    }
}

impl std::fmt::Display for ScriptNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which host role a script grant was made for.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScriptHostRole {
    #[default]
    Owner,
    Follower,
}

impl ScriptHostRole {
    pub fn as_str(self) -> &'static str {
        match self {
            ScriptHostRole::Owner => "owner",
            ScriptHostRole::Follower => "follower",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(ScriptHostRole::Owner),
            "follower" => Some(ScriptHostRole::Follower),
            _ => None,
        }
    }
}

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

/// A script manifest validation refusal. The message is written for the
/// operator who can open the manifest and fix it; the catalog loader adds
/// the manifest's file path around it.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScriptValidationError {
    #[error(
        "invalid script name `{0}` (want a lowercase letter then lowercase letters, digits, `_` or `-`, at most 64 bytes)"
    )]
    InvalidName(String),
    #[error("script `entry` must be a non-empty workspace-relative path, got `{0}`")]
    InvalidEntry(String),
    #[error("script interpreter element {index} is empty")]
    EmptyInterpreterElement { index: usize },
    #[error(
        "script interpreter `{program}` argument `{flag}` runs inline code, which a script manifest may not carry; put the code in the entry file"
    )]
    InlineCodeFlag { program: String, flag: String },
    #[error("script interpreter arguments are literal and may not contain templates: `{0}`")]
    TemplatedInterpreter(String),
    #[error("script argument `{0}` has an unbalanced or empty template")]
    InvalidArgTemplate(String),
    #[error(
        "script entry `#!` line is not an honored form (`#!/abs/path [one-arg]` or `#!/usr/bin/env <name>`): `{0}`"
    )]
    InvalidShebang(String),
    #[error(
        "script request `env_pass` name `{0}` is not a usable environment variable name, or names an `ORBIT_*` variable, which is never passed"
    )]
    InvalidEnvPass(String),
    #[error("script request `timeout_seconds` must be at least 1")]
    InvalidTimeout,
    #[error("script file input `as` must be a plain file name, got `{0}`")]
    InvalidFileInputName(String),
}

/// Whether `name` may name a script (its manifest file is `<name>.yaml`).
pub fn is_valid_script_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The inline-code spellings refused for one interpreter program basename,
/// if the program is one whose inline form is known. The tuple is
/// (inline short-option letters, inline long options, other short-option
/// letters that take an argument). An argument-taking letter ends a
/// single-dash cluster: the rest of that element is its argument, so the
/// `c` in `python3 -Xpycache_prefix=x` and the `e` in `perl -MExporter`
/// are argument text, not inline flags.
fn inline_code_flag(
    program_basename: &str,
) -> Option<(&'static [u8], &'static [&'static str], &'static [u8])> {
    match program_basename {
        "sh" | "bash" | "zsh" | "dash" => Some((b"c", &[], b"o")),
        "perl" => Some((b"eE", &[], b"0CDFIMim")),
        "ruby" => Some((b"e", &[], b"0CEFIKTWri")),
        "node" => Some((b"ep", &["--eval", "--print"], b"r")),
        other if other.starts_with("python") => Some((b"c", &[], b"mWX")),
        _ => None,
    }
}

fn program_basename(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Refuse an interpreter argv that could run inline code instead of the
/// entry file. Every element is checked: a known interpreter named
/// anywhere in a launcher list may not be followed by its inline flag, in
/// the separate (`-c`), glued (`-c…`), bundled (`-ec`, `-xc`) or long
/// (`--eval`) spelling.
pub fn validate_script_interpreter(interpreter: &[String]) -> Result<(), ScriptValidationError> {
    for (index, element) in interpreter.iter().enumerate() {
        if element.is_empty() {
            return Err(ScriptValidationError::EmptyInterpreterElement { index });
        }
        if element.contains("{{") {
            return Err(ScriptValidationError::TemplatedInterpreter(element.clone()));
        }
    }
    for (index, element) in interpreter.iter().enumerate() {
        let Some((inline_letters, long_flags, argument_letters)) =
            inline_code_flag(program_basename(element))
        else {
            continue;
        };
        for argument in &interpreter[index + 1..] {
            if argument == "--" {
                break;
            }
            if let Some(long) = argument.strip_prefix("--") {
                let is_inline = long_flags.iter().any(|flag| {
                    long == flag.trim_start_matches('-')
                        || long
                            .strip_prefix(flag.trim_start_matches('-'))
                            .is_some_and(|rest| rest.starts_with('='))
                });
                if is_inline {
                    return Err(ScriptValidationError::InlineCodeFlag {
                        program: element.clone(),
                        flag: argument.clone(),
                    });
                }
                continue;
            }
            let Some(cluster) = argument.strip_prefix('-').filter(|rest| !rest.is_empty()) else {
                continue;
            };
            for letter in cluster.bytes() {
                if inline_letters.contains(&letter) {
                    return Err(ScriptValidationError::InlineCodeFlag {
                        program: element.clone(),
                        flag: argument.clone(),
                    });
                }
                if argument_letters.contains(&letter) {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// The interpreter argv an entry `#!` line declares.
///
/// Honored forms are `#!/abs/path`, `#!/abs/path <one-arg>` and
/// `#!/usr/bin/env <name>`; the result is treated exactly as if it were
/// written in the manifest's `interpreter:` list, so the same inline-code
/// refusal applies. `Ok(None)` means the first line is not a shebang and
/// the entry is a native executable.
pub fn script_interpreter_from_shebang(
    first_line: &str,
) -> Result<Option<Vec<String>>, ScriptValidationError> {
    let Some(rest) = first_line.strip_prefix("#!") else {
        return Ok(None);
    };
    let rest = rest.trim();
    let mut parts = rest.splitn(2, char::is_whitespace);
    let program = parts.next().unwrap_or_default();
    let argument = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if !program.starts_with('/') {
        return Err(ScriptValidationError::InvalidShebang(
            first_line.to_string(),
        ));
    }
    let interpreter = if program == "/usr/bin/env" {
        let Some(name) = argument else {
            return Err(ScriptValidationError::InvalidShebang(
                first_line.to_string(),
            ));
        };
        if name.contains(char::is_whitespace) || name.starts_with('-') {
            return Err(ScriptValidationError::InvalidShebang(
                first_line.to_string(),
            ));
        }
        vec![name.to_string()]
    } else {
        let mut interpreter = vec![program.to_string()];
        if let Some(argument) = argument {
            interpreter.push(argument.to_string());
        }
        interpreter
    };
    validate_script_interpreter(&interpreter)?;
    Ok(Some(interpreter))
}

/// Check one manifest argument's templates: every `{{ … }}` is balanced
/// and non-empty. A rendered value still becomes exactly one argv element.
pub fn validate_script_arg_template(argument: &str) -> Result<(), ScriptValidationError> {
    let mut rest = argument;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(ScriptValidationError::InvalidArgTemplate(
                argument.to_string(),
            ));
        };
        if after_open[..close].trim().is_empty() {
            return Err(ScriptValidationError::InvalidArgTemplate(
                argument.to_string(),
            ));
        }
        rest = &after_open[close + 2..];
    }
    Ok(())
}

/// Validate a parsed script manifest and its metadata name, without
/// touching the filesystem. `entry` resolution (the path must stay inside
/// the workspace) is the catalog loader's half of this contract.
pub fn validate_script_spec(name: &str, spec: &ScriptSpec) -> Result<(), ScriptValidationError> {
    if !is_valid_script_name(name) {
        return Err(ScriptValidationError::InvalidName(name.to_string()));
    }
    if spec.entry.trim().is_empty() || spec.entry.starts_with('/') {
        return Err(ScriptValidationError::InvalidEntry(spec.entry.clone()));
    }
    validate_script_interpreter(&spec.interpreter)?;
    for argument in &spec.args {
        validate_script_arg_template(argument)?;
    }
    for input in &spec.files.inputs {
        validate_script_arg_template(&input.from)?;
        let as_name = input.as_.as_str();
        if as_name.is_empty() || as_name.contains('/') || as_name == ".." || as_name == "." {
            return Err(ScriptValidationError::InvalidFileInputName(
                input.as_.clone(),
            ));
        }
    }
    for env_name in &spec.requests.env_pass {
        let valid = !env_name.is_empty()
            && !env_name.starts_with("ORBIT_")
            && env_name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && env_name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(ScriptValidationError::InvalidEnvPass(env_name.clone()));
        }
    }
    if spec.requests.timeout_seconds == Some(0) {
        return Err(ScriptValidationError::InvalidTimeout);
    }
    Ok(())
}

/// The Linux script profile a `script_execution` record names.
pub const SCRIPT_PROFILE_LINUX: &str = "linux-bwrap-script-v1";

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
    /// A granted root, or the run's cwd tree, is missing or unusable.
    pub const ROOT_UNAVAILABLE: &str = "script_root_unavailable";
    /// A pull follower holds no grant made with `--follower`.
    pub const FOLLOWER_REFUSED: &str = "script_follower_refused";
    /// The manifest in the run's checkout no longer loads.
    pub const MANIFEST_INVALID: &str = "script_manifest_invalid";
    /// The step input fails the manifest's `input_schema_json`, or a
    /// templated argument does not render to one safe argv element.
    pub const INPUT_INVALID: &str = "script_input_invalid";
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
    #[serde(default)]
    pub network: ScriptNetwork,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_roots: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_roots: Vec<PathBuf>,
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
}
