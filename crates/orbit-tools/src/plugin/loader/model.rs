use super::*;

/// Manifest digests of first-party plugins that may claim `orbit.<ns>.*`
/// without a constellation-works source. Empty until a release bundles one.
pub const FIRST_PARTY_MANIFEST_DIGESTS: &[&str] = &[];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginLoadError {
    #[error("{0}")]
    Manifest(#[from] PluginManifestError),
    /// A filesystem operation reading the plugin tree failed. `kind` is kept
    /// alongside the formatted message so the `OrbitError` conversion can
    /// tell a caller-fixable "no such file" from a host-side
    /// `PermissionDenied` refusal, instead of collapsing every read failure
    /// into the same error shape.
    #[error("{message}")]
    Io {
        message: String,
        kind: std::io::ErrorKind,
    },
    /// A plugin tree entry is a symbolic link (§4.9): fail-closed security
    /// policy, not a data problem with the manifest.
    #[error("{0}")]
    SymlinkRefused(String),
}

impl PluginLoadError {
    pub(super) fn io(message: String, kind: std::io::ErrorKind) -> Self {
        Self::Io { message, kind }
    }
}

impl From<PluginLoadError> for OrbitError {
    fn from(error: PluginLoadError) -> Self {
        match &error {
            PluginLoadError::Manifest(_) => OrbitError::InvalidInput(error.to_string()),
            PluginLoadError::SymlinkRefused(_) => OrbitError::PolicyDenied(error.to_string()),
            // `NotFound` is ordinary bad input (a path that does not name a
            // plugin); `PermissionDenied` is the host's own access control
            // refusing the read, which is a policy refusal, not invalid
            // input. Anything else is a genuine I/O failure.
            PluginLoadError::Io { kind, .. } => match kind {
                std::io::ErrorKind::NotFound => OrbitError::InvalidInput(error.to_string()),
                std::io::ErrorKind::PermissionDenied => OrbitError::PolicyDenied(error.to_string()),
                _ => OrbitError::Io(error.to_string()),
            },
        }
    }
}

/// A manifest rejection is invalid input at every surface that surfaces it.
pub fn manifest_refusal(error: PluginManifestError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

/// One `spec.tools[]` entry with its schemas resolved and flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPluginTool {
    /// The manifest verb.
    pub verb: String,
    pub description: String,
    pub execution_kind: PluginExecutionKind,
    pub mcp_scope: PluginMcpScope,
    pub input_schema: Value,
    /// Whether the manifest declared `input_schema` (as opposed to the
    /// empty-object default). An `mcp` backend's server is held to a
    /// declared schema only.
    pub input_schema_declared: bool,
    /// The declared `output_schema`, compiled at load: every call validates
    /// its backend's output against this validator without recompiling.
    pub output_schema: Option<CompiledSchema>,
    pub parameters: Vec<ToolParam>,
}

/// The definition files one plugin ships, resolved to absolute paths inside
/// the plugin root (§4.5). Activities and jobs become the `plugin:<ns>`
/// catalog layer; routines and auto-tasks are seeded on enable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginDefinitionFiles {
    pub activities: Vec<PathBuf>,
    pub jobs: Vec<PathBuf>,
    pub routines: Vec<PathBuf>,
    pub auto_tasks: Vec<PathBuf>,
}

impl PluginDefinitionFiles {
    pub fn is_empty(&self) -> bool {
        self.activities.is_empty()
            && self.jobs.is_empty()
            && self.routines.is_empty()
            && self.auto_tasks.is_empty()
    }
}

/// A plugin directory whose manifest parsed, whose `$ref`s resolved inside
/// the root, and whose backend command exists.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPlugin {
    pub root: PathBuf,
    pub manifest: PluginManifest,
    pub manifest_digest: String,
    pub backend_command: PathBuf,
    pub tools: Vec<ResolvedPluginTool>,
    /// `spec.definitions.*` resolved to files inside the root.
    pub definitions: PluginDefinitionFiles,
    /// `spec.skills[]` resolved to directories holding a `SKILL.md`.
    pub skills: Vec<PathBuf>,
    /// `spec.config.schema` resolved and read, when the manifest declares one.
    pub config_schema: Option<Value>,
    /// `spec.config.defaults`, flattened to one entry per declared key.
    pub config_defaults: BTreeMap<String, Value>,
    /// `spec.tests[]` resolved to golden files inside the root, parsed and
    /// structurally validated (§5). Empty when the manifest ships none.
    pub tests: Vec<LoadedPluginTestFile>,
}

/// One resolved `spec.tests` file and its parsed cases.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedPluginTestFile {
    pub path: PathBuf,
    pub file: PluginTestFile,
}

impl LoadedPlugin {
    pub fn namespace(&self) -> &str {
        &self.manifest.metadata.name
    }

    /// Canonical registry name of one of this plugin's tools.
    pub fn tool_name(&self, verb: &str, first_party: bool) -> String {
        plugin_tool_name(self.namespace(), verb, first_party)
    }
}

/// SHA-256 of the manifest bytes, hex encoded.
pub fn manifest_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Walk `root` without following links and refuse any symbolic link, naming
/// the relative entry.
///
/// `std::fs::copy` follows a link and writes the *target's* bytes as a
/// regular file. The install root is always readable to the plugin backend,
/// so a link to `/proc/self/environ` (or any other readable path) would leak
/// into a location the sandbox is supposed to hide.
pub fn refuse_plugin_tree_symlinks(root: &Path) -> Result<(), PluginLoadError> {
    refuse_plugin_tree_symlinks_in(root, root)
}

fn refuse_plugin_tree_symlinks_in(root: &Path, dir: &Path) -> Result<(), PluginLoadError> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        PluginLoadError::io(format!("read {}: {error}", dir.display()), error.kind())
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            PluginLoadError::io(format!("read {}: {error}", dir.display()), error.kind())
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            PluginLoadError::io(format!("stat {}: {error}", path.display()), error.kind())
        })?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).ok();
            return Err(PluginLoadError::SymlinkRefused(plugin_symlink_refusal(
                path.strip_prefix(root).unwrap_or(&path),
                target.as_deref(),
            )));
        }
        if file_type.is_dir() {
            refuse_plugin_tree_symlinks_in(root, &path)?;
        }
    }
    Ok(())
}

/// Diagnostic that names the offending plugin-tree entry.
pub fn plugin_symlink_refusal(entry: &Path, target: Option<&Path>) -> String {
    match target {
        Some(target) => format!(
            "refusing '{}': it is a symbolic link to '{}'; plugin trees cannot contain \
             symbolic links because they would be copied as regular files into the install root",
            entry.display(),
            target.display()
        ),
        None => format!(
            "refusing '{}': it is a symbolic link; plugin trees cannot contain symbolic \
             links because they would be copied as regular files into the install root",
            entry.display()
        ),
    }
}
