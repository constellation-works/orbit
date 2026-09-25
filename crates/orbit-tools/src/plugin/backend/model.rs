use super::*;

/// Host ceiling on `spec.backend.timeout_ms`.
pub const PLUGIN_TIMEOUT_CEILING_MS: u64 = 300_000;

/// Store directories under Orbit's global root that a confined `orbit tool
/// run` appends to. This is the same inventory the agent sandbox grants a
/// nested Orbit process (`orbit-core`'s `append_linux_runtime_write_roots`),
/// and it is deliberately *not* the root itself: `bin/orbit` is executed
/// unconfined by the scheduler and every worker, `plugins/` records what each
/// plugin is allowed to do, and `config.toml`, `mcp-callers.toml` and
/// `clock.toml` are host configuration. Those stay readable and unwritable
/// [ORB-12777].
pub(super) const ORBIT_TOOLS_GLOBAL_WRITE_DIRS: &[&str] = &["state/logs", "state/audit", "tasks"];

/// Individual files under the global root the child opens for writing: the
/// audit/task store's WAL file set, and the executable-generation locks every
/// `orbit` process pins before it bootstraps.
///
/// Each is granted only when it already exists as a regular file. A Landlock
/// rule binds an inode, so a name that is not there yet cannot be granted —
/// and materialising one here would be a write no grant made. The host
/// process that spawns the backend has already opened the store and pinned
/// its own generation, so the file set is present for the call.
pub(super) const ORBIT_TOOLS_GLOBAL_WRITE_FILES: &[&str] = &[
    "orbit.db",
    "orbit.db-wal",
    "orbit.db-shm",
    ".generation.lock",
    ".generation-admission.lock",
];

/// Host-owned trees beneath the global root that no plugin child may read,
/// whatever else it was granted.
///
/// `state/plugin-callbacks/` holds the live callback credentials: a plugin
/// that could read the directory could present another plugin's token, and
/// one that could list it could enumerate every backend running on the host.
/// A confined child is granted its *own* record as a single file instead
/// ([`PluginSandboxProfile::with_callback_session`]), which is what lets the
/// child check that the credential descriptor it inherited really is the
/// record the host wrote for it. `plugins/.grants/` holds the
/// grant witnesses that decide what each plugin is authorized to do; those are
/// the host's answer, never a plugin's input, and the child is likewise
/// granted only its own ([`plugin_grant_witness_relative`]) so it can verify
/// its own row and read nothing about any other plugin [ORB-12798].
pub(super) const PLUGIN_GLOBAL_READ_DENY_DIRS: &[&str] =
    &["state/plugin-callbacks", "plugins/.grants"];

/// Host-owned directory beside the namespace install directories, holding one
/// grant-authorization witness per plugin (`orbit-core`'s `plugin_grants`).
/// Named here because the sandbox decides who may read it.
pub const PLUGIN_GRANT_WITNESS_DIR: &str = ".grants";

/// Where `plugin`'s witness sits, relative to the global root.
pub fn plugin_grant_witness_relative(plugin: &str) -> PathBuf {
    Path::new("plugins")
        .join(PLUGIN_GRANT_WITNESS_DIR)
        .join(format!("{plugin}.json"))
}

/// The same narrowing for the workspace's `.orbit/`: the stores a callback
/// writes, never `plugins.yaml` (the install pin), `routines/`, `auto_tasks/`
/// or `config.toml`.
pub(super) const ORBIT_TOOLS_WORKSPACE_WRITE_DIRS: &[&str] = &[
    "tasks",
    "frictions",
    "state/audit",
    "state/logs",
    "state/job-runs",
];

/// The workspace lexical index's WAL file set, granted on the same terms as
/// [`ORBIT_TOOLS_GLOBAL_WRITE_FILES`]. Callback session files live at
/// `state/plugin-callbacks/` under the global root, which is absent from
/// both write inventories: the host writes them, and the child must not.
pub(super) const ORBIT_TOOLS_WORKSPACE_WRITE_FILES: &[&str] = &[
    "state/semantic.db",
    "state/semantic.db-wal",
    "state/semantic.db-shm",
];

/// The effective `[plugins.<ns>]` section: what the operator configured over
/// the manifest's declared defaults, checked against the plugin's own schema
/// before the host built this spec (§1).
///
/// A section may hold credentials — a plugin that talks to a paid API is
/// configured with its token like any other key — so it reaches the backend
/// process and nothing else. `Debug` prints the key names and never a value,
/// which keeps the section out of a log line that formats a spec, and no
/// caller formats it by hand.
#[derive(Clone, PartialEq)]
pub struct PluginConfigSection(Value);

impl Default for PluginConfigSection {
    /// A plugin that declares no keys and is configured with none still has a
    /// section: an empty object, so a backend reads `context.config` the same
    /// way whether or not the operator wrote a `[plugins.<ns>]` table.
    fn default() -> Self {
        Self(Value::Object(serde_json::Map::new()))
    }
}

impl PluginConfigSection {
    /// Wrap an already-resolved section. The host resolves it once, so both
    /// dispatch surfaces read the same object.
    pub fn new(section: Value) -> Self {
        Self(section)
    }

    /// The section as the backend receives it, with every JSON type intact.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    /// The declared keys, in section order. Values are deliberately absent.
    pub fn keys(&self) -> Vec<&str> {
        self.0
            .as_object()
            .map(|values| values.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The same section rendered for `{{config.<key>}}`, where a template can
    /// only interpolate text: strings as themselves, every other JSON scalar
    /// as its JSON form.
    pub fn rendered_values(&self) -> BTreeMap<String, String> {
        self.0
            .as_object()
            .map(|values| {
                values
                    .iter()
                    .map(|(key, value)| {
                        let rendered = match value {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        (key.clone(), rendered)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl fmt::Debug for PluginConfigSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PluginConfigSection")
            .field(&self.keys())
            .finish()
    }
}

/// The per-plugin facts a backend needs to run one of its tools.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBackendSpec {
    pub provenance: PluginProvenance,
    pub plugin_root: PathBuf,
    /// `ORBIT_PLUGIN_STATE`: the host's per-plugin state directory.
    pub state_dir: PathBuf,
    /// Orbit's own global root. A backend granted `orbit_tools` calls
    /// `orbit tool run`, which reads this root and the workspace's `.orbit/`
    /// and appends to some of the stores beneath them, so the sandbox opens
    /// them for that grant and for no other — the roots read-only, the stores
    /// by name (see [`ORBIT_TOOLS_GLOBAL_WRITE_DIRS`]).
    pub global_root: PathBuf,
    /// The resolved backend program and its fixed arguments.
    pub command: PathBuf,
    pub args: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub sandbox: PluginSandbox,
    /// The manifest's requests; every one of them the host has granted.
    pub permissions: PluginPermissions,
    /// `spec.requires.programs`: what the backend declares it spawns.
    pub programs: Vec<String>,
    /// The effective `[plugins.<ns>]` section. One resolution serves the
    /// backend's own view of its configuration — `context.config` on both
    /// dispatch surfaces — and the manifest's `{{config.<key>}}` templates,
    /// so the two cannot disagree.
    pub config: PluginConfigSection,
    /// The grants recorded at enable time, with the roots `fs` was scoped to
    /// when the operator named them.
    pub grants: PluginGrantSet,
}

/// Manifest-declared filesystem roots after template rendering and resolution
/// against the plugin install directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFsRoots {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
}

/// Render both filesystem permission lists through one path-resolution rule.
///
/// Relative roots belong to the plugin that declared them, so every phase
/// resolves them against `plugin_root`; they must never inherit the host
/// process's current working directory.
pub fn render_fs_roots(
    spec: &PluginBackendSpec,
    vars: &PluginTemplateVars,
) -> Result<RenderedFsRoots, PluginManifestError> {
    Ok(RenderedFsRoots {
        read: render_root_list(
            &spec.permissions.fs.read,
            vars,
            &spec.plugin_root,
            "spec.permissions.fs.read",
        )?,
        write: render_root_list(
            &spec.permissions.fs.write,
            vars,
            &spec.plugin_root,
            "spec.permissions.fs.write",
        )?,
    })
}

/// Render one list of declared roots under the rule [`render_fs_roots`]
/// documents.
///
/// Shared with the operator's `--grant fs=<root>,…` list, which is written in
/// the same template language as the manifest and must resolve identically:
/// the profile compiler compares the two path sets, and a root that resolved
/// one way on the request side and another on the grant side would compare as
/// disjoint and silently drop the access the operator meant to allow.
pub(super) fn render_root_list(
    declared: &[String],
    vars: &PluginTemplateVars,
    plugin_root: &Path,
    field: &str,
) -> Result<Vec<PathBuf>, PluginManifestError> {
    declared
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            let rendered = render_template(declared, vars, &format!("{field}[{index}]"))?;
            let path = PathBuf::from(rendered);
            Ok(if path.is_absolute() {
                path
            } else {
                plugin_root.join(path)
            })
        })
        .collect()
}

/// One filesystem root the sandbox will actually open, beside the manifest
/// entry it serves so a refusal can name what was asked for.
pub(super) struct EffectiveRoot {
    pub(super) path: PathBuf,
    pub(super) field: String,
    pub(super) declared: String,
}

/// A manifest root the operator's grant left out entirely.
///
/// The plugin will fail to reach it at run time, which is the intended
/// outcome of scoping a grant — but it is also the shape of an honest
/// mistake, so every surface that can name it does: a log line at call time
/// and an `orbit plugin doctor` row before the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedFsRoot {
    /// `spec.permissions.fs.write[1]`.
    pub field: String,
    /// The root as the manifest declares it, before rendering.
    pub declared: String,
}

/// Narrow the manifest's rendered request to what the operator granted.
///
/// `granted` is `None` for the manifest-request shorthand (`--grant fs`), and
/// the request passes through unchanged — the behaviour every recorded `fs`
/// grant had before roots existed. When the operator named roots, each
/// requested root is kept only where it overlaps one of them, and the
/// *narrower* side of the overlap is what the profile gets: a request for
/// `{{workspace}}` under a grant of `{{workspace}}/.orbit-graph` opens the one
/// directory, never the workspace (design §4.3).
///
/// A requested root that overlaps nothing granted is dropped with a
/// diagnostic rather than refused. The manifest asked for more than this host
/// allows, which is the operator's decision working as intended; refusing
/// would make a narrower grant equivalent to no grant, and the plugin would
/// be unusable exactly when the operator scoped it most carefully.
pub(super) fn scope_fs_roots(
    requested: &[PathBuf],
    declared: &[String],
    granted: Option<&[PathBuf]>,
    field: &str,
    dropped: &mut Vec<DroppedFsRoot>,
) -> Vec<EffectiveRoot> {
    let mut effective: Vec<EffectiveRoot> = Vec::new();
    let mut push = |path: PathBuf, index: usize| {
        if effective.iter().any(|root| root.path == path) {
            return;
        }
        effective.push(EffectiveRoot {
            path,
            field: format!("{field}[{index}]"),
            declared: declared.get(index).cloned().unwrap_or_default(),
        });
    };
    for (index, request) in requested.iter().enumerate() {
        let Some(granted) = granted else {
            push(request.clone(), index);
            continue;
        };
        let mut overlapped = false;
        for allowed in granted {
            if let Some(intersection) = root_intersection(request, allowed) {
                overlapped = true;
                push(intersection, index);
            }
        }
        if !overlapped {
            dropped.push(DroppedFsRoot {
                field: format!("{field}[{index}]"),
                declared: declared.get(index).cloned().unwrap_or_default(),
            });
        }
    }
    effective
}

/// The narrower of two roots when one contains the other, or `None` when they
/// are disjoint.
///
/// Both sides are resolved the way the sandbox compiles its rules before they
/// are compared, so a symlink cannot make a granted root appear to contain a
/// request it does not ([`physical_with_missing_tail`]). The *unresolved*
/// winner is returned: it resolves to the same place, and keeping the path the
/// operator or manifest wrote keeps the profile readable in a diagnostic.
fn root_intersection(requested: &Path, granted: &Path) -> Option<PathBuf> {
    let request = physical_with_missing_tail(requested);
    let allowed = physical_with_missing_tail(granted);
    if request == allowed || request.starts_with(&allowed) {
        Some(requested.to_path_buf())
    } else if allowed.starts_with(&request) {
        Some(granted.to_path_buf())
    } else {
        None
    }
}
