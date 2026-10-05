use super::*;

/// What the host knows when it decides whether a loaded plugin may register.
#[derive(Debug, Clone)]
pub struct PluginValidationPolicy {
    /// The source resolved to a constellation-works repository, or the
    /// manifest digest is bundled as first-party.
    pub first_party_verified: bool,
    /// Every built-in tool name, from a registry with builtins registered.
    pub builtin_tools: Vec<String>,
    pub reserved_cli_commands: &'static [&'static str],
}

impl PluginValidationPolicy {
    /// The policy for a host that has not verified the plugin's origin.
    pub fn host_default() -> Self {
        let mut registry = ToolRegistry::new();
        registry.register_builtins();
        Self {
            first_party_verified: false,
            builtin_tools: registry
                .all_schemas()
                .into_iter()
                .map(|schema| schema.name)
                .collect(),
            reserved_cli_commands: RESERVED_CLI_COMMANDS,
        }
    }

    pub fn with_first_party_verified(mut self, verified: bool) -> Self {
        self.first_party_verified = verified;
        self
    }
}

/// Apply the namespace rules (§1): first-party claims need verification,
/// and a namespace may not shadow a built-in tool or a CLI command.
pub fn validate_loaded_plugin(
    plugin: &LoadedPlugin,
    policy: &PluginValidationPolicy,
) -> Result<(), PluginManifestError> {
    let namespace = plugin.namespace();
    if plugin.manifest.claims_first_party_namespace() {
        if plugin.manifest.metadata.publisher.as_deref() != Some(FIRST_PARTY_PUBLISHER) {
            return Err(PluginManifestError::new(
                "metadata.publisher",
                format!(
                    "`origin: orbit` requires `publisher: {FIRST_PARTY_PUBLISHER}`; this manifest \
                     declares '{}'",
                    plugin.manifest.metadata.publisher.as_deref().unwrap_or("")
                ),
            ));
        }
        if !policy.first_party_verified {
            return Err(PluginManifestError::new(
                "metadata.origin",
                format!(
                    "`origin: orbit` claims the reserved `orbit.{namespace}.*` namespace, but the \
                     plugin source is not a constellation-works repository and its manifest digest \
                     is not in the bundled first-party list; remove `origin` to register as \
                     `{namespace}.*`"
                ),
            ));
        }
    }
    let first_party = plugin.manifest.claims_first_party_namespace();
    if policy.reserved_cli_commands.contains(&namespace) {
        return Err(PluginManifestError::new(
            "metadata.name",
            format!(
                "'{namespace}' is a built-in `orbit {namespace}` command and cannot be a plugin namespace"
            ),
        ));
    }
    if let Some(builtin) = policy
        .builtin_tools
        .iter()
        .find(|builtin| namespace_collides_with_tool(namespace, first_party, builtin))
    {
        return Err(PluginManifestError::new(
            "metadata.name",
            format!(
                "namespace '{}' collides with the built-in tool '{builtin}'",
                if first_party {
                    format!("orbit.{namespace}")
                } else {
                    namespace.to_string()
                }
            ),
        ));
    }
    for (index, tool) in plugin.tools.iter().enumerate() {
        let name = plugin.tool_name(&tool.verb, first_party);
        if policy.builtin_tools.contains(&name) {
            return Err(PluginManifestError::new(
                format!("spec.tools[{index}].name"),
                format!("'{name}' is a built-in tool"),
            ));
        }
    }
    Ok(())
}

/// Refuse `spec.permissions.fs.write` roots that contain the plugin install
/// tree or Orbit's global root, that select a protected path inside the
/// global root, or that reach a workspace's `.orbit` / `.git` metadata.
///
/// A write tree on `{{plugin_root}}` (or any parent) lets the backend rewrite
/// `plugin.yaml` under an already-recorded `fs` grant; a write tree on the
/// global root does the same to the host install. A narrower write inside the
/// global root can still replace host executables, grant witnesses, or another
/// plugin's files, so only the current plugin's `{{plugin_state}}` tree is
/// allowed there. Paths that need `{{workspace}}` are rendered against a
/// synthetic root here, so validation and registration can enforce the
/// workspace-relative rule before a concrete workspace is selected. Call time
/// repeats the check against the real workspace root.
pub fn refuse_covering_fs_write_roots(
    spec: &PluginBackendSpec,
    workspace_root: Option<&Path>,
) -> Result<(), PluginManifestError> {
    // Validation and registration do not have a selected workspace. A stable
    // absolute sentinel preserves every path relationship beneath
    // `{{workspace}}` without borrowing any real host path.
    let validation_workspace = Path::new("/__orbit_plugin_workspace__");
    let workspace_root = workspace_root.unwrap_or(validation_workspace);
    let vars = spec.template_vars(Some(workspace_root));
    let roots = render_fs_roots(spec, &vars)?;
    for (index, absolute) in roots.write.iter().enumerate() {
        let field = format!("spec.permissions.fs.write[{index}]");
        if let Some(protected) = fs_write_root_covers(
            absolute,
            &spec.plugin_root,
            &spec.global_root,
            &spec.state_dir,
            Some(workspace_root),
        ) {
            return Err(PluginManifestError::new(
                field,
                format!(
                    "'{}' grants write access to the {protected}; a plugin cannot request \
                     writes to its own install tree or anywhere beneath Orbit's global root \
                     except its own plugin state tree, or to workspace metadata `.orbit` / `.git`",
                    spec.permissions
                        .fs
                        .write
                        .get(index)
                        .map(String::as_str)
                        .unwrap_or("")
                ),
            ));
        }
    }
    Ok(())
}

/// Whether `write` reaches a host path a plugin must not modify. Equality
/// counts: a grant on the plugin root itself is how a backend rewrites
/// `plugin.yaml`. Within the global root, only the current plugin's state tree
/// is writable. A workspace grant must neither contain nor sit inside
/// `.orbit` or `.git`; component comparisons mean `.orbit-graph` remains an
/// ordinary workspace directory.
///
/// Every side of every comparison is resolved with
/// [`physical_with_missing_tail`], the same resolution the sandbox compiles
/// its rules under: a root whose tail does not exist yet is still read at the
/// place its existing ancestors physically live, so an alias a backend
/// planted in its own writable state cannot present a protected install tree
/// as a path inside `{{plugin_state}}` [ORB-12799].
pub fn fs_write_root_covers(
    write: &Path,
    plugin_root: &Path,
    global_root: &Path,
    plugin_state: &Path,
    workspace_root: Option<&Path>,
) -> Option<&'static str> {
    let write = physical_with_missing_tail(write);
    let plugin_root = physical_with_missing_tail(plugin_root);
    let global_root = physical_with_missing_tail(global_root);
    let plugin_state = physical_with_missing_tail(plugin_state);
    if is_path_prefix(&write, &plugin_root) {
        return Some("plugin install root");
    } else if is_path_prefix(&write, &global_root) {
        return Some("Orbit global root");
    } else if is_path_prefix(&global_root, &write) && !is_path_prefix(&plugin_state, &write) {
        return Some("protected path beneath Orbit global root");
    } else if let Some(workspace_root) = workspace_root {
        let workspace_root = physical_with_missing_tail(workspace_root);
        let workspace_orbit = physical_with_missing_tail(&workspace_root.join(".orbit"));
        let workspace_git = physical_with_missing_tail(&workspace_root.join(".git"));
        if is_path_prefix(&write, &workspace_orbit)
            || is_path_prefix(&workspace_orbit, &write)
            || is_path_prefix(&write, &workspace_git)
            || is_path_prefix(&workspace_git, &write)
        {
            return Some("workspace metadata paths `.orbit` / `.git`");
        }
    }
    None
}

fn is_path_prefix(prefix: &Path, path: &Path) -> bool {
    path == prefix || path.starts_with(prefix)
}

/// Whether `source` (the `orbit plugin add` argument) is a
/// constellation-works repository fetched through a `git+` URL.
///
/// A directory's Git configuration belongs to the plugin author, so it is
/// never evidence of a first-party source.
pub fn first_party_source(source: &str) -> bool {
    source
        .strip_prefix("git+")
        .is_some_and(is_first_party_remote)
}

fn is_first_party_remote(url: &str) -> bool {
    match remote_host_and_org(url) {
        Some((host, org)) => host == "github.com" && org == FIRST_PARTY_PUBLISHER,
        None => false,
    }
}

/// Parse a git remote URL's host and first path segment (its organisation),
/// handling both `scheme://[user@]host[:port]/org/repo` and the SCP-like
/// `[user@]host:org/repo` form `git@github.com:org/repo` uses. Returns `None`
/// when `url` does not decompose into a host and a non-empty first segment,
/// so an unrecognised shape fails closed rather than matching by substring.
fn remote_host_and_org(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches(".git").trim_end_matches('/');
    if let Some((_scheme, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = host.split(':').next().unwrap_or(host);
        let org = path
            .split('/')
            .next()
            .filter(|segment| !segment.is_empty())?;
        return Some((host.to_lowercase(), org.to_string()));
    }
    let (host_part, path) = url.split_once(':')?;
    if host_part.is_empty() || host_part.contains('/') {
        return None;
    }
    let host = host_part
        .rsplit_once('@')
        .map_or(host_part, |(_, host)| host);
    let org = path
        .split('/')
        .next()
        .filter(|segment| !segment.is_empty())?;
    Some((host.to_lowercase(), org.to_string()))
}
