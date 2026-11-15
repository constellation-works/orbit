use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ScriptFileOutputs;

/// `script:<name>` prefix for the `target:` field on a [`super::super::TargetRef`].
pub const SCRIPT_REF_PREFIX: &str = "script:";

/// The only `schemaVersion` a script manifest may declare.
pub const SCRIPT_MANIFEST_SCHEMA_VERSION: u32 = 1;

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
///
/// `from` is templated against the step input and must resolve physically
/// inside the cwd tree or a granted read root, outside the masked trees and
/// the credential denies; otherwise the step refuses before spawn with
/// [`super::script_refusal::FILE_INPUT_UNAVAILABLE`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ScriptFileInput {
    pub from: String,
    #[serde(rename = "as")]
    pub as_: String,
}

/// Domain separator for a directory input's tree digest.
pub const SCRIPT_INPUT_TREE_DOMAIN: &str = "orbit.script.input-tree.v1";

/// The most entries one directory input may hold; a larger tree refuses the
/// step rather than be hashed without bound.
pub const SCRIPT_INPUT_MAX_TREE_ENTRIES: usize = 100_000;

/// What one declared input was when the attempt admitted it: recorded on the
/// `script.execution` entry beside the output files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptInputFile {
    /// The `as` name: the input appears at `in/<name>`.
    pub name: String,
    /// The physical source path that was mounted.
    pub source: PathBuf,
    /// `file` or `directory`.
    pub kind: String,
    /// Bytes of a file input, or the sum of a directory's regular files.
    pub bytes: u64,
    /// SHA-256 of a file's bytes, or for a directory the tree digest over
    /// every entry's relative path, kind and content (lowercase hex).
    pub sha256: String,
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
