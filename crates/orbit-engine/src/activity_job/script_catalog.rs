//! Script manifest loading for `target: script:<name>`, and the argument
//! rendering a script step runs with.
//!
//! A script is a manifest at `<scripts_dir>/<name>.yaml` (`schemaVersion:
//! 1`, `kind: Script`) whose `spec.entry` names the program file by a
//! workspace-relative path. Loading a manifest validates the spec, then
//! resolves `entry` physically: it must be a regular file beneath the
//! workspace root, outside `.git/` and `.orbit/state/`. Every refusal
//! names the manifest file, so a job step that names a missing, escaping
//! or inline-code script fails to load with the file to fix. The digests a
//! grant binds are computed here, over the manifest and entry bytes as
//! loaded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use orbit_types::resource::{ResourceKind, ResourceMetadata};
use orbit_types::workflow::activity_job::{
    SCRIPT_GRANT_DIGEST_DOMAIN, ScriptSpec, ScriptValidationError, script_interpreter_from_shebang,
    script_sha256_hex, validate_script_spec,
};

/// One loaded script manifest plus what grant and dispatch decisions are
/// made from: where it came from, where its entry resolved, the effective
/// interpreter argv, and the digests of both files.
#[derive(Debug, Clone)]
pub struct ScriptAsset {
    pub name: String,
    pub spec: ScriptSpec,
    pub manifest_path: PathBuf,
    /// The entry path after physical resolution, beneath the workspace root.
    pub entry_path: PathBuf,
    /// The manifest's `interpreter:` list, or the argv its entry's `#!`
    /// line declares, or empty for a native executable.
    pub interpreter: Vec<String>,
    /// Combined grant digest over manifest and entry bytes.
    pub digest: String,
    pub manifest_digest: String,
    pub entry_digest: String,
}

/// A script manifest load refusal. Every variant names the manifest file.
#[derive(Debug, Error)]
pub enum ScriptLoadError {
    #[error("read script manifest {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("read dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse script manifest {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("script manifest {path}: {source}")]
    Validation {
        path: PathBuf,
        source: ScriptValidationError,
    },
    #[error(
        "script manifest {path}: metadata.name `{name}` does not match the file name; a script's manifest is `<name>.yaml`"
    )]
    NameMismatch { path: PathBuf, name: String },
    #[error(
        "script manifest {path}: entry `{entry}` does not resolve to a regular file inside the workspace ({reason})"
    )]
    EntryEscapes {
        path: PathBuf,
        entry: String,
        reason: String,
    },
    #[error("script manifest {path}: read entry {entry}: {source}")]
    ReadEntry {
        path: PathBuf,
        entry: PathBuf,
        source: std::io::Error,
    },
    #[error("duplicate script name `{name}` — defined in both {first} and {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
}

#[derive(Debug, serde::Deserialize)]
struct ScriptEnvelopeYaml {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    kind: ResourceKind,
    metadata: ResourceMetadata,
    spec: ScriptSpec,
}

/// Load and validate one script manifest file against `workspace_root`.
pub fn load_script_asset(
    manifest_path: &Path,
    workspace_root: &Path,
) -> Result<ScriptAsset, ScriptLoadError> {
    load_script_snapshot(manifest_path, workspace_root).map(|snapshot| snapshot.asset)
}

/// A loaded manifest plus the exact manifest and entry bytes its digests
/// were computed over. A script step writes these bytes, and only these,
/// into its sandbox, so a write to either file after the grant check
/// changes nothing for the run that checked it.
#[derive(Debug, Clone)]
pub struct ScriptSnapshot {
    pub asset: ScriptAsset,
    pub manifest_bytes: Vec<u8>,
    pub entry_bytes: Vec<u8>,
}

/// [`load_script_asset`], keeping the bytes each file was read as.
pub fn load_script_snapshot(
    manifest_path: &Path,
    workspace_root: &Path,
) -> Result<ScriptSnapshot, ScriptLoadError> {
    let manifest_bytes =
        std::fs::read(manifest_path).map_err(|source| ScriptLoadError::ReadManifest {
            path: manifest_path.to_path_buf(),
            source,
        })?;
    let envelope: ScriptEnvelopeYaml =
        serde_yaml::from_slice(&manifest_bytes).map_err(|error| ScriptLoadError::Parse {
            path: manifest_path.to_path_buf(),
            message: error.to_string(),
        })?;
    if envelope.schema_version
        != orbit_types::workflow::activity_job::SCRIPT_MANIFEST_SCHEMA_VERSION
    {
        return Err(ScriptLoadError::Parse {
            path: manifest_path.to_path_buf(),
            message: format!(
                "unsupported schemaVersion: {} (want {})",
                envelope.schema_version,
                orbit_types::workflow::activity_job::SCRIPT_MANIFEST_SCHEMA_VERSION
            ),
        });
    }
    if envelope.kind != ResourceKind::Script {
        return Err(ScriptLoadError::Parse {
            path: manifest_path.to_path_buf(),
            message: format!("kind mismatch: expected `Script`, got `{}`", envelope.kind),
        });
    }
    let name = envelope.metadata.name;
    if manifest_path.file_stem().and_then(|stem| stem.to_str()) != Some(name.as_str()) {
        return Err(ScriptLoadError::NameMismatch {
            path: manifest_path.to_path_buf(),
            name,
        });
    }
    let spec = envelope.spec;
    validate_script_spec(&name, &spec).map_err(|source| ScriptLoadError::Validation {
        path: manifest_path.to_path_buf(),
        source,
    })?;
    let entry_path = resolve_script_entry(manifest_path, workspace_root, &spec.entry)?;
    // The digests are over both files' bytes as loaded, so this one read
    // of the entry is what the grant binding is made from.
    let entry_bytes = std::fs::read(&entry_path).map_err(|source| ScriptLoadError::ReadEntry {
        path: manifest_path.to_path_buf(),
        entry: entry_path.clone(),
        source,
    })?;
    let first_line = entry_bytes
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| std::str::from_utf8(line).ok())
        .map(|line| line.trim_end_matches('\r'));
    let interpreter = if !spec.interpreter.is_empty() {
        spec.interpreter.clone()
    } else {
        match first_line {
            Some(line) => script_interpreter_from_shebang(line)
                .map_err(|source| ScriptLoadError::Validation {
                    path: manifest_path.to_path_buf(),
                    source,
                })?
                .unwrap_or_default(),
            None => Vec::new(),
        }
    };
    let entry_digest = script_sha256_hex(&entry_bytes);
    let manifest_digest = script_sha256_hex(&manifest_bytes);
    let digest =
        orbit_types::workflow::activity_job::script_content_digest(&manifest_bytes, &entry_bytes);
    Ok(ScriptSnapshot {
        asset: ScriptAsset {
            name,
            spec,
            manifest_path: manifest_path.to_path_buf(),
            entry_path,
            interpreter,
            digest,
            manifest_digest,
            entry_digest,
        },
        manifest_bytes,
        entry_bytes,
    })
}

/// The step output an exit-0 script answered with, or why it is invalid
/// (§4): stdout within the cap, UTF-8, one JSON document (`{}` for empty
/// stdout without a schema) that passes `output_schema_json`. The host
/// asks the same question before it keeps any output file, so a set is
/// only ever stored for an output the step accepts.
pub fn script_stdout_output(
    stdout: &[u8],
    over_cap: bool,
    schema: &Value,
) -> Result<Value, String> {
    if over_cap {
        return Err(format!(
            "stdout is larger than its {} byte cap",
            orbit_types::workflow::activity_job::SCRIPT_STDOUT_CAP_BYTES
        ));
    }
    let text = std::str::from_utf8(stdout)
        .map_err(|error| format!("stdout is not UTF-8: {error}"))?
        .trim();
    let has_schema = !schema.is_null();
    let output = if text.is_empty() && !has_schema {
        Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str::<Value>(text)
            .map_err(|error| format!("stdout is not one JSON document: {error}"))?
    };
    if has_schema {
        let validator = orbit_tools::plugin::CompiledSchema::compile(schema.clone())
            .map_err(|error| format!("output_schema_json {error}"))?;
        if let Some(violations) = validator.violations(&output) {
            return Err(format!("stdout fails output_schema_json: {violations}"));
        }
    }
    Ok(output)
}

/// Why a templated script argument did not render.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ScriptArgError {
    #[error("script argument `{argument}`: template `{{{{ {expression} }}}}` {reason}")]
    Template {
        argument: String,
        expression: String,
        reason: String,
    },
    #[error(
        "script argument `{argument}` renders to `{value}`, which starts with `-`; give the template a literal prefix (such as `--label={{{{ input.label }}}}`) so input cannot become an option"
    )]
    OptionInjection { argument: String, value: String },
}

/// Render a manifest's `args` against the step input (§1). Each element
/// renders to exactly one argv element: every `{{ input.<path> }}` must
/// name a string, number or boolean in the input, a value is never split
/// or globbed, and an element whose rendered text starts with `-` because
/// of a template at its very start is refused.
pub fn render_script_args(args: &[String], input: &Value) -> Result<Vec<String>, ScriptArgError> {
    args.iter()
        .map(|argument| render_script_arg(argument, input))
        .collect()
}

fn render_script_arg(argument: &str, input: &Value) -> Result<String, ScriptArgError> {
    let mut rendered = String::new();
    let mut rest = argument;
    let mut leading_template = false;
    while let Some(open) = rest.find("{{") {
        rendered.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            break;
        };
        let expression = after_open[..close].trim();
        let template_error = |reason: &str| ScriptArgError::Template {
            argument: argument.to_string(),
            expression: expression.to_string(),
            reason: reason.to_string(),
        };
        if rendered.is_empty() {
            leading_template = true;
        }
        let Some(path) = expression.strip_prefix("input.") else {
            return Err(template_error("must read `input.<field>`"));
        };
        let mut value = input;
        for segment in path.split('.') {
            value = value
                .get(segment)
                .ok_or_else(|| template_error("names no value in the step input"))?;
        }
        match value {
            Value::String(text) => rendered.push_str(text),
            Value::Number(number) => rendered.push_str(&number.to_string()),
            Value::Bool(flag) => rendered.push_str(if *flag { "true" } else { "false" }),
            _ => {
                return Err(template_error("must render to a string, number or boolean"));
            }
        }
        rest = &after_open[close + 2..];
    }
    rendered.push_str(rest);
    if leading_template && rendered.starts_with('-') {
        return Err(ScriptArgError::OptionInjection {
            argument: argument.to_string(),
            value: rendered,
        });
    }
    Ok(rendered)
}

/// Resolve `entry` physically beneath `workspace_root`, outside `.git/`
/// and `.orbit/state/`. A symlink that leaves the workspace resolves to
/// its target and is refused there, not at the link.
fn resolve_script_entry(
    manifest_path: &Path,
    workspace_root: &Path,
    entry: &str,
) -> Result<PathBuf, ScriptLoadError> {
    let escapes = |reason: String| ScriptLoadError::EntryEscapes {
        path: manifest_path.to_path_buf(),
        entry: entry.to_string(),
        reason,
    };
    let root = workspace_root
        .canonicalize()
        .map_err(|error| escapes(format!("workspace root does not resolve: {error}")))?;
    let candidate = root.join(entry);
    let resolved = candidate
        .canonicalize()
        .map_err(|error| escapes(format!("does not resolve: {error}")))?;
    if !resolved.starts_with(&root) {
        return Err(escapes(format!(
            "resolves to {}, outside {}",
            resolved.display(),
            root.display()
        )));
    }
    for forbidden in [root.join(".git"), root.join(".orbit").join("state")] {
        if let Ok(forbidden) = forbidden.canonicalize()
            && resolved.starts_with(&forbidden)
        {
            return Err(escapes(format!(
                "resolves under {}, which a script entry may not live in",
                forbidden.display()
            )));
        }
    }
    let metadata = std::fs::metadata(&resolved)
        .map_err(|error| escapes(format!("does not resolve: {error}")))?;
    if !metadata.is_file() {
        return Err(escapes("is not a regular file".to_string()));
    }
    Ok(resolved)
}

/// The workspace's script catalog: every manifest in one scripts
/// directory, keyed by name. Scripts are workspace-only, so there is one
/// directory and no layering.
#[derive(Debug, Clone)]
pub struct V2ScriptCatalog {
    scripts_dir: PathBuf,
    workspace_root: PathBuf,
    entries: BTreeMap<String, ScriptAsset>,
}

impl V2ScriptCatalog {
    /// An empty catalog over `scripts_dir`; call [`Self::load_dir`] to read
    /// the manifests in it. An empty catalog still knows the manifest path
    /// a missing script name would live at, which is what the
    /// missing-manifest load error names.
    pub fn new(scripts_dir: PathBuf, workspace_root: PathBuf) -> Self {
        Self {
            scripts_dir,
            workspace_root,
            entries: BTreeMap::new(),
        }
    }

    /// Load every `*.yaml` / `*.yml` manifest directly in the scripts
    /// directory. A missing directory is an empty catalog, not an error:
    /// most workspaces have no scripts. One invalid manifest refuses the
    /// whole load, naming that file.
    pub fn load_dir(&mut self) -> Result<(), ScriptLoadError> {
        let read_dir = match std::fs::read_dir(&self.scripts_dir) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(ScriptLoadError::ReadDir {
                    path: self.scripts_dir.clone(),
                    source,
                });
            }
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(|source| ScriptLoadError::ReadDir {
                path: self.scripts_dir.clone(),
                source,
            })?;
            let path = entry.path();
            let is_yaml = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension == "yaml" || extension == "yml");
            if is_yaml && path.is_file() {
                paths.push(path);
            }
        }
        paths.sort();
        for path in paths {
            let asset = load_script_asset(&path, &self.workspace_root)?;
            if let Some(existing) = self.entries.get(&asset.name) {
                return Err(ScriptLoadError::DuplicateName {
                    name: asset.name,
                    first: existing.manifest_path.clone(),
                    second: path,
                });
            }
            self.entries.insert(asset.name.clone(), asset);
        }
        Ok(())
    }

    /// Insert one already-loaded asset — used by smokes and tests.
    pub fn insert(&mut self, asset: ScriptAsset) -> Option<ScriptAsset> {
        self.entries.insert(asset.name.clone(), asset)
    }

    pub fn get(&self, name: &str) -> Option<&ScriptAsset> {
        self.entries.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ScriptAsset> {
        self.entries.values()
    }

    pub fn scripts_dir(&self) -> &Path {
        &self.scripts_dir
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Where the manifest for `name` lives or would live.
    pub fn manifest_path_for(&self, name: &str) -> PathBuf {
        self.scripts_dir.join(format!("{name}.yaml"))
    }
}

// Keep the digest domain reachable beside the catalog so a reader tracing
// the grant binding finds it from the loader as well as from the types.
pub const SCRIPT_DIGEST_DOMAIN: &str = SCRIPT_GRANT_DIGEST_DOMAIN;
