use super::*;

impl PluginBackendSpec {
    pub fn granted(&self, grant: PluginGrant) -> bool {
        self.grants.contains(grant)
    }

    /// The effective timeout for one call, capped by the host ceiling.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
            .unwrap_or(TIMEOUT_SLOW_MS)
            .min(PLUGIN_TIMEOUT_CEILING_MS)
    }

    /// The effective section rendered for text surfaces: manifest templates
    /// and the dashboard tiles that show what the plugin runs with.
    pub fn config_values(&self) -> BTreeMap<String, String> {
        self.config.rendered_values()
    }

    pub(crate) fn template_vars(&self, workspace_root: Option<&Path>) -> PluginTemplateVars {
        PluginTemplateVars {
            workspace: workspace_root.map(|path| path.to_string_lossy().into_owned()),
            plugin_root: self.plugin_root.to_string_lossy().into_owned(),
            plugin_state: self.state_dir.to_string_lossy().into_owned(),
            config: self.config.rendered_values(),
        }
    }

    /// The filesystem and network boundary for a call made from
    /// `workspace_root`: the manifest's fs paths rendered, plus the plugin
    /// root itself, which holds the backend the child must execute.
    pub fn sandbox_profile(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<PluginSandboxProfile, OrbitError> {
        let vars = self.template_vars(workspace_root);
        let roots = render_fs_roots(self, &vars).map_err(plugin_refusal)?;
        // The operator's roots, rendered under the manifest's own rule. They
        // narrow both lists: one `--grant fs=<roots>` scopes reads and writes
        // together, because a root a plugin may read is the same kind of
        // decision as one it may write.
        let granted_roots = match self.grants.fs_roots() {
            Some(declared) => Some(
                render_root_list(declared, &vars, &self.plugin_root, "--grant fs")
                    .map_err(plugin_refusal)?,
            ),
            None => None,
        };
        let read_denies: Vec<PathBuf> = PLUGIN_GLOBAL_READ_DENY_DIRS
            .iter()
            .map(|relative| self.global_root.join(relative))
            .collect();
        let mut dropped = Vec::new();
        let mut read = vec![self.plugin_root.clone()];
        if self.granted(PluginGrant::Fs) {
            for root in scope_fs_roots(
                &roots.read,
                &self.permissions.fs.read,
                granted_roots.as_deref(),
                "spec.permissions.fs.read",
                &mut dropped,
            ) {
                // Both platforms grant a read root that sits *inside* a
                // denied tree — that is how the host hands a child its own
                // record — so a manifest root there would buy back another
                // plugin's state, session or witness. Only the host names
                // those re-allows; a manifest root inside a denied tree is
                // dropped unless it is within this plugin's own state.
                if self.inside_foreign_denied_tree(&root.path, &read_denies) {
                    tracing::warn!(
                        target: "orbit.tools.plugin",
                        plugin = %self.provenance.name,
                        field = %root.field,
                        requested = %root.declared,
                        "plugin requests a read root inside host-owned Orbit state (another \
                         plugin's state, the callback sessions or the grant witnesses); it is \
                         not on the sandbox profile and no grant can add it",
                    );
                    continue;
                }
                read.push(root.path);
            }
        }
        let scoped_write = if self.granted(PluginGrant::Fs) {
            scope_fs_roots(
                &roots.write,
                &self.permissions.fs.write,
                granted_roots.as_deref(),
                "spec.permissions.fs.write",
                &mut dropped,
            )
        } else {
            Vec::new()
        };
        for root in &dropped {
            tracing::warn!(
                target: "orbit.tools.plugin",
                plugin = %self.provenance.name,
                field = %root.field,
                requested = %root.declared,
                "plugin requests a filesystem root outside the roots this host granted; it is \
                 not on the sandbox profile. Re-run `orbit plugin enable` with a `--grant \
                 fs=<root>` list covering it if the access is intended.",
            );
        }
        // Run on the *effective* roots, not the requested ones: narrowing can
        // only shrink a tree, but the granted side supplies the path when it
        // is the narrower one, so this is the last point at which a protected
        // path could reach the profile.
        for root in &scoped_write {
            if let Some(protected) = super::super::loader::fs_write_root_covers(
                &root.path,
                &self.plugin_root,
                &self.global_root,
                &self.state_dir,
                workspace_root,
            ) {
                return Err(plugin_refusal(PluginManifestError::new(
                    root.field.clone(),
                    format!(
                        "'{}' grants write access to the {protected}; a plugin cannot request \
                         writes to its own install tree or anywhere beneath Orbit's global root \
                         except its own plugin state tree, or to workspace metadata `.orbit` / `.git`",
                        root.declared
                    ),
                )));
            }
        }
        let mut write: Vec<PathBuf> = scoped_write.into_iter().map(|root| root.path).collect();
        let mut write_files = Vec::new();
        // The write directories the *host itself* adds for a grant, as opposed
        // to the ones the manifest asked for above. They are also exactly the
        // `orbit_tools` half of the materialization roots below: one list
        // decides both, so an entry added to either inventory is granted and
        // creatable in the same edit and can never again be granted-but-absent
        // — the drift that reddened CI four times [ORB-12872].
        let mut host_write_dirs = Vec::new();
        if self.granted(PluginGrant::OrbitTools) {
            // The callback runs `orbit tool run` in the child: its own
            // governance, allowlist and audit decide what that call may do,
            // but it cannot run at all without reading Orbit's roots and
            // appending to Orbit's stores. Those are two different sets. The
            // roots are granted read-only and the stores are named one by
            // one, so holding `orbit_tools` no longer carries the right to
            // rewrite the binary the scheduler runs unconfined, the recorded
            // plugin installs, or the MCP authorization ceiling [ORB-12777].
            read.push(self.global_root.clone());
            // Its own witness, and no other plugin's: the nested `orbit tool
            // run` verifies the grants recorded for this plugin before it
            // registers the row, and the witness directory itself is denied
            // above. A confined child therefore cannot read what any other
            // plugin was authorized for [ORB-12798].
            read.push(
                self.global_root
                    .join(plugin_grant_witness_relative(&self.provenance.name)),
            );
            for relative in ORBIT_TOOLS_GLOBAL_WRITE_DIRS {
                host_write_dirs.push(self.global_root.join(relative));
            }
            for relative in ORBIT_TOOLS_GLOBAL_WRITE_FILES {
                write_files.push(self.global_root.join(relative));
            }
            if let Some(workspace_root) = workspace_root {
                let workspace_orbit = workspace_root.join(".orbit");
                for relative in ORBIT_TOOLS_WORKSPACE_WRITE_DIRS {
                    host_write_dirs.push(workspace_orbit.join(relative));
                }
                for relative in ORBIT_TOOLS_WORKSPACE_WRITE_FILES {
                    write_files.push(workspace_orbit.join(relative));
                }
                read.push(workspace_orbit);
            }
        }
        write.extend(host_write_dirs.iter().cloned());
        // The child's own state tree, re-allowed inside the denied
        // `state/plugins/` on every profile: the one directory the standard
        // gives a plugin for durable state stays readable to it and to no
        // other plugin. Writing it is still the `fs.write` grant's decision
        // above; this only keeps the carve-out from taking a plugin's own
        // state away from it.
        if !read.contains(&self.state_dir) {
            read.push(self.state_dir.clone());
        }
        let network = if self.granted(PluginGrant::Network) {
            self.permissions.network
        } else {
            PluginNetworkPermission::None
        };
        Ok(PluginSandboxProfile {
            read,
            read_denies,
            state_dir: self.state_dir.clone(),
            write,
            write_files,
            // The whole host-materialized prefix set, stated once: the
            // selected workspace, this plugin's own state tree, and every
            // directory the host itself put in `write` just above. Nothing
            // here is a wider tree than the grant it serves — in particular
            // the global root's `state/` is *not* a prefix, only the two
            // named stores under it are.
            materialization_roots: workspace_root
                .into_iter()
                .map(Path::to_path_buf)
                .chain(std::iter::once(self.state_dir.clone()))
                .chain(host_write_dirs)
                .collect(),
            network,
            unsandboxed: self.sandbox == PluginSandbox::None
                && self.granted(PluginGrant::Unsandboxed),
            // Set by `with_callback_session` once the session exists.
            callback_fd: None,
        })
    }

    /// Whether `path` resolves at or beneath a denied tree without resolving
    /// into this plugin's own state tree.
    fn inside_foreign_denied_tree(&self, path: &Path, read_denies: &[PathBuf]) -> bool {
        let path = physical_with_missing_tail(path);
        let own_state = physical_with_missing_tail(&self.state_dir);
        if path.starts_with(&own_state) {
            return false;
        }
        read_denies
            .iter()
            .any(|denied| path.starts_with(physical_with_missing_tail(denied)))
    }

    /// The manifest roots the operator's `fs` scope leaves out, for a surface
    /// that reports them before a call rather than after one
    /// ([`DroppedFsRoot`]).
    ///
    /// Empty whenever `fs` is unscoped or ungranted: the shorthand drops
    /// nothing, and a plugin without the grant has no fs profile to narrow.
    /// A render error is reported as no drops — the call itself refuses on it,
    /// with a better message than a doctor row could give.
    pub fn dropped_fs_roots(&self, workspace_root: Option<&Path>) -> Vec<DroppedFsRoot> {
        if !self.granted(PluginGrant::Fs) {
            return Vec::new();
        }
        let vars = self.template_vars(workspace_root);
        let Some(declared) = self.grants.fs_roots() else {
            return Vec::new();
        };
        let (Ok(roots), Ok(granted)) = (
            render_fs_roots(self, &vars),
            render_root_list(declared, &vars, &self.plugin_root, "--grant fs"),
        ) else {
            return Vec::new();
        };
        let mut dropped = Vec::new();
        scope_fs_roots(
            &roots.read,
            &self.permissions.fs.read,
            Some(&granted),
            "spec.permissions.fs.read",
            &mut dropped,
        );
        scope_fs_roots(
            &roots.write,
            &self.permissions.fs.write,
            Some(&granted),
            "spec.permissions.fs.write",
            &mut dropped,
        );
        dropped
    }

    /// The child environment for one call: the allowlisted baseline, the
    /// granted `env_pass` names copied from this process, and the Orbit
    /// plugin variables (design §4.2). `tool_name` is absent for a
    /// long-lived `mcp` child, which serves every tool.
    ///
    /// `env_pass` is composed through the same admission path as the
    /// baseline (`allowlisted_child_env_from`), not a raw name lookup: a
    /// manifest names a variable to request it, but the excluded
    /// privilege-bearing `ORBIT_*` names (`ORBIT_OPERATOR`,
    /// `ORBIT_WORKSPACE_CLAIM_TOKEN`) can never ride that request through
    /// even if present in the parent environment (`validate_structure`
    /// refuses them at the manifest too; this is the defense-in-depth
    /// boundary for a manifest loaded before that check existed).
    pub fn child_environment(
        &self,
        ctx: &ToolContext,
        cwd: &str,
        tool_name: Option<&str>,
    ) -> Vec<(String, String)> {
        let mut env_pairs = ctx
            .proc_spawn_environment
            .clone()
            .unwrap_or_else(|| allowlisted_child_env(&[], &[]));
        if self.granted(PluginGrant::EnvPass) {
            let parent = ctx
                .proc_spawn_environment
                .clone()
                .unwrap_or_else(|| std::env::vars().collect());
            let admitted = allowlisted_child_env_from(&parent, &self.permissions.env_pass, &[]);
            for name in &self.permissions.env_pass {
                if let Some((_, value)) = admitted
                    .iter()
                    .find(|(admitted_name, _)| admitted_name == name)
                {
                    upsert_env(&mut env_pairs, name, value.clone());
                }
            }
        }
        let mut set = |key: &str, value: String| upsert_env(&mut env_pairs, key, value);
        set("ORBIT_HOST_API", PLUGIN_HOST_API.to_string());
        set("ORBIT_VERSION", env!("CARGO_PKG_VERSION").to_string());
        set("ORBIT_PLUGIN", self.provenance.name.clone());
        set("ORBIT_PLUGIN_VERSION", self.provenance.version.clone());
        set(
            "ORBIT_PLUGIN_ROOT",
            self.plugin_root.to_string_lossy().into_owned(),
        );
        set(
            "ORBIT_PLUGIN_STATE",
            self.state_dir.to_string_lossy().into_owned(),
        );
        if let Some(tool_name) = tool_name {
            set("ORBIT_TOOL_NAME", tool_name.to_string());
        }
        set("ORBIT_TOOL_CWD", cwd.to_string());
        if let Some(workspace_root) = ctx.workspace_root.as_ref() {
            set(
                "ORBIT_WORKSPACE_ROOT",
                workspace_root.to_string_lossy().into_owned(),
            );
        }
        // Always present, even when empty: information for the backend.
        // The callback gate does not read this value; identity is the session
        // record the child inherits on `PLUGIN_CALLBACK_FD`.
        set("ORBIT_ALLOWED_TOOLS", self.allowed_tools(ctx).join(","));
        // `requires.programs` is what a callback through `proc.spawn` may run.
        set("ORBIT_PROC_ALLOWED_PROGRAMS", self.programs.join(","));
        env_pairs
    }

    /// `permissions.orbit_tools` ∩ the `orbit_tools` grant ∩ the caller's own
    /// allowlist when it has one (§4.2).
    pub fn allowed_tools(&self, ctx: &ToolContext) -> Vec<String> {
        if !self.granted(PluginGrant::OrbitTools) {
            return Vec::new();
        }
        self.permissions
            .orbit_tools
            .iter()
            .filter(|tool| {
                ctx.allowed_tools.is_empty()
                    || ctx.allowed_tools.iter().any(|allowed| allowed == *tool)
            })
            .cloned()
            .collect()
    }

    /// A caller whose own context restricts programs (an activity-scoped
    /// run) bounds what the plugin may spawn: every program the manifest
    /// declares must be on the caller's list, checked through the same gate
    /// `proc.spawn` uses.
    pub fn enforce_programs(&self, ctx: &ToolContext, tool_name: &str) -> Result<(), OrbitError> {
        for program in &self.programs {
            enforce_program_allowlist(ctx, tool_name, program)?;
        }
        Ok(())
    }
}

/// A manifest problem discovered while building the call-time sandbox
/// boundary — a template that cannot render, or a requested root that covers
/// a path a plugin may never write — is the host's security boundary
/// refusing the call, not malformed caller input.
fn plugin_refusal(error: PluginManifestError) -> OrbitError {
    OrbitError::PolicyDenied(error.to_string())
}
