//! Convergence of stored pre-family tool names [ORB-23552].
//!
//! Tool families [ORB-23280] made an operation such as
//! `orbit.task.artifact.put` an internal identity: a stored tool list names
//! the family action (`orbit.task.artifact:put`) instead. Records written
//! before that change still hold operation names. Admission reads them as
//! family actions ([`orbit_types::tool::tool_requirement_name`]) for the
//! compatibility window; this module rewrites the stored records so they
//! converge and that read path can retire:
//!
//! - auto-task definitions (`template.required_tools` in shipped, forked and
//!   workspace-local YAML), rewritten in place so comments and layout survive.
//!   A file the managed manifest tracks keeps its provenance: the manifest
//!   records the rewritten bytes, so the rename never turns a managed
//!   definition into a fork;
//! - task `required_tools` and `granted_tools`, rewritten in the bundle with a
//!   `tool_names_migrated` history event;
//! - installed plugin manifests' `permissions.orbit_tools`, rewritten only in
//!   a manifest whose bytes still carry the digest this host recorded at
//!   install, which then records the rewritten digest.
//!
//! The mapping is [`orbit_types::tool::family_action_tool_names`]. Every
//! rewrite runs once on upgrade (the first writable runtime open) and again
//! wherever Orbit meets an old name: admission, auto-task load and mint, and
//! `orbit workspace sync`.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all, with_exclusive_file_lock};
use orbit_common::protocol::yaml::parse_auto_task_yaml;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::Task;
use orbit_types::tool::{family_action_tool_names, is_tool_family_operation};
use serde::Serialize;

use crate::OrbitRuntime;
use crate::application::auto_tasks::settings::AUTO_TASK_ASSET_KIND;
use crate::application::auto_tasks::{auto_tasks_dir, cursor_state_path};
use crate::application::managed_assets::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetLayout, load_managed_asset_manifest,
    restore_managed_asset,
};

/// The actor a migration's task history event names.
pub(crate) const TOOL_NAMES_MIGRATION_ACTOR: &str = "system:tool-names-migration";

/// The ledger, under a root's `state/` directory, that records each file
/// rewrite: one JSON object per line.
pub(crate) const TOOL_NAME_MIGRATION_LEDGER: &str = "tool-name-migrations.jsonl";

/// The marker, under a root's `state/` directory, that records the upgrade
/// pass completed for that root.
const TOOL_NAME_MIGRATION_MARKER: &str = "tool-family-names-v1.migrated";

/// How many task envelopes the upgrade pass reads at a time.
const TASK_ENVELOPE_BATCH: usize = 256;

/// One stored record whose pre-family tool names were rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolNameRewrite {
    /// `auto_task` or `plugin`.
    pub kind: &'static str,
    /// The definition or plugin name.
    pub name: String,
    pub path: PathBuf,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

impl ToolNameRewrite {
    /// One line for a sync report or a warning.
    pub fn describe(&self) -> String {
        format!(
            "pre-family tool names rewritten to family actions: {} → {}",
            self.before.join(", "),
            self.after.join(", ")
        )
    }
}

/// Rewrite the operation names of the YAML list under `key` in `raw`: a block
/// sequence below the key or a flow sequence on its line. Everything else,
/// including comments and prose elsewhere, is left byte for byte. Callers
/// verify the parsed result, so a layout this does not recognize is refused
/// rather than half rewritten.
fn rewrite_yaml_list_tool_names(raw: &str, key: &str) -> String {
    let mut rewritten = String::with_capacity(raw.len());
    let mut list_indent: Option<usize> = None;
    for line in raw.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        let indent = content.len() - trimmed.len();
        if let Some(key_indent) = list_indent {
            let continues = trimmed.is_empty()
                || trimmed.starts_with('#')
                || indent > key_indent
                || (indent == key_indent && trimmed.starts_with('-'));
            if !continues {
                list_indent = None;
            }
        }
        let is_key = trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        if is_key {
            list_indent = Some(indent);
        }
        if list_indent.is_some() {
            rewritten.push_str(&rewrite_line_tool_names(line));
        } else {
            rewritten.push_str(line);
        }
    }
    rewritten
}

/// Replace each operation name on `line` that stands as a whole list item:
/// after `-`, `[` or `,` and before `,`, `]`, `#` or the line end, optionally
/// quoted.
fn rewrite_line_tool_names(line: &str) -> String {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let mut rewritten = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(is_name_char) {
        let (before, tail) = rest.split_at(start);
        let end = tail.find(|c: char| !is_name_char(c)).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        rewritten.push_str(before);
        let opened = rewritten
            .trim_end()
            .trim_end_matches(['"', '\''])
            .trim_end()
            .ends_with(['-', '[', ',']);
        let closed = after
            .trim_start_matches(['"', '\''])
            .trim_start()
            .chars()
            .next()
            .is_none_or(|next| matches!(next, ',' | ']' | '#' | '\n' | '\r'));
        if opened && closed && is_tool_family_operation(token) {
            rewritten.push_str(&orbit_types::tool::tool_requirement_name(token));
        } else {
            rewritten.push_str(token);
        }
        rest = after;
    }
    rewritten.push_str(rest);
    rewritten
}

/// Rewrite one auto-task definition file whose `template.required_tools`
/// names a pre-family operation, and keep a tracked file managed. `None` when
/// the file needs nothing, cannot be parsed (doctor reports it faulty), or its
/// layout cannot be rewritten in place exactly (logged and left alone).
/// `write: false` reports the rewrite without making it.
///
/// The caller holds the workspace auto-task cursor lock when `write`.
pub(crate) fn heal_auto_task_definition_file(
    dir: &Path,
    path: &Path,
    write: bool,
) -> Result<Option<ToolNameRewrite>, OrbitError> {
    let raw = fs::read_to_string(path).map_err(|error| {
        OrbitError::io_with_context(&error, format!("read {}: {error}", path.display()))
    })?;
    let Ok(definition) = parse_auto_task_yaml(&raw) else {
        return Ok(None);
    };
    let Some(after) = family_action_tool_names(&definition.template.required_tools) else {
        return Ok(None);
    };
    let healed = rewrite_yaml_list_tool_names(&raw, "required_tools");
    let mut expected = definition.clone();
    expected.template.required_tools = after.clone();
    if parse_auto_task_yaml(&healed).ok().as_ref() != Some(&expected) {
        tracing::warn!(
            target: "orbit.core.tool_names",
            path = %path.display(),
            "auto-task definition names pre-family tools in a layout that cannot be rewritten \
             in place; admission still reads them as family actions"
        );
        return Ok(None);
    }
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(definition.name.as_str())
        .to_string();
    if write {
        let tracked = load_managed_asset_manifest(
            &dir.join(MANAGED_ASSET_MANIFEST_FILE),
            AUTO_TASK_ASSET_KIND,
            ManagedAssetLayout::YamlStem,
        )?
        .and_then(|manifest| manifest.assets.get(&name).cloned());
        atomic_write_text(path, &healed).map_err(|error| {
            OrbitError::io_with_context(&error, format!("rewrite {}: {error}", path.display()))
        })?;
        // The manifest proves Orbit wrote the old bytes; the rename is
        // Orbit's too, so the file stays managed rather than reading as a
        // fork.
        if tracked.as_deref() == Some(sha256_hex(raw.as_bytes()).as_str()) {
            restore_managed_asset(
                dir,
                AUTO_TASK_ASSET_KIND,
                ManagedAssetLayout::YamlStem,
                &name,
                sha256_hex(healed.as_bytes()),
            )?;
        }
    }
    Ok(Some(ToolNameRewrite {
        kind: "auto_task",
        name,
        path: path.to_path_buf(),
        before: definition.template.required_tools,
        after,
    }))
}

/// [`heal_auto_task_definition_file`] over every regular `*.yaml` file in
/// `<orbit_root>/auto_tasks`. A file that fails to read is logged and
/// skipped, so one bad file never stops the rest converging.
pub(crate) fn heal_auto_task_catalog(
    orbit_root: &Path,
    write: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    let dir = auto_tasks_dir(orbit_root);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        _ => return Ok(Vec::new()),
    }
    let mut paths = fs::read_dir(&dir)
        .map_err(|error| {
            OrbitError::io_with_context(&error, format!("read {}: {error}", dir.display()))
        })?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .collect::<Vec<_>>();
    paths.sort();
    let mut rewrites = Vec::new();
    for path in paths {
        match heal_auto_task_definition_file(&dir, &path, write) {
            Ok(Some(rewrite)) => rewrites.push(rewrite),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                target: "orbit.core.tool_names",
                path = %path.display(),
                %error,
                "skipped rewriting pre-family tool names in an auto-task definition"
            ),
        }
    }
    if write {
        record_tool_name_rewrites(&orbit_root.join("state"), &rewrites);
    }
    Ok(rewrites)
}

/// Append each rewrite to the root's ledger and log it. Recording is
/// bookkeeping: a failure is logged, never fails the rewrite it describes.
pub(crate) fn record_tool_name_rewrites(state_dir: &Path, rewrites: &[ToolNameRewrite]) {
    if rewrites.is_empty() {
        return;
    }
    for rewrite in rewrites {
        tracing::info!(
            target: "orbit.core.tool_names",
            kind = rewrite.kind,
            name = rewrite.name.as_str(),
            path = %rewrite.path.display(),
            before = ?rewrite.before,
            after = ?rewrite.after,
            "rewrote stored pre-family tool names to family actions"
        );
    }
    let ledger = state_dir.join(TOOL_NAME_MIGRATION_LEDGER);
    let at = Utc::now().to_rfc3339();
    let result = (|| {
        create_private_dir_all(state_dir)?;
        let mut content = match fs::read_to_string(&ledger) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        for rewrite in rewrites {
            let line = serde_json::json!({ "at": at, "rewrite": rewrite });
            content.push_str(&line.to_string());
            content.push('\n');
        }
        atomic_write_text(&ledger, &content)
    })();
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.tool_names",
            path = %ledger.display(),
            %error,
            "skipped recording tool name rewrites"
        );
    }
}

fn task_needs_tool_name_migration(required: &[String], granted: &[String]) -> bool {
    required
        .iter()
        .chain(granted)
        .any(|name| is_tool_family_operation(name))
}

impl OrbitRuntime {
    /// Rewrite the pre-family tool names in this checkout's auto-task
    /// definitions under the cursor lock, and record each rewrite.
    pub(crate) fn heal_local_auto_task_tool_names(
        &self,
    ) -> Result<Vec<ToolNameRewrite>, OrbitError> {
        let local_dir = self.paths().local_dir.clone();
        if heal_auto_task_catalog(&local_dir, false)?.is_empty() {
            return Ok(Vec::new());
        }
        let state_path = cursor_state_path(&self.paths().state_dir);
        with_exclusive_file_lock(&state_path, "auto-task cursor", || {
            heal_auto_task_catalog(&local_dir, true)
        })
    }

    /// [`Self::heal_local_auto_task_tool_names`] for a read or mint path:
    /// a failure is logged, never fails the caller, which still reads old
    /// names as family actions.
    pub(crate) fn heal_local_auto_task_tool_names_best_effort(&self) {
        if let Err(error) = self.heal_local_auto_task_tool_names() {
            tracing::warn!(
                target: "orbit.core.tool_names",
                %error,
                "skipped rewriting pre-family tool names in auto-task definitions"
            );
        }
    }

    /// Rewrite `task`'s stored pre-family tool names to family actions when
    /// it holds any, recording the rewrite in its history. Returns whether
    /// the stored task changed. A host that cannot write the task (a
    /// follower, a read-only store) leaves it to the owner; admission reads
    /// the old names as family actions either way.
    pub(crate) fn heal_task_tool_names(&self, task: &Task) -> bool {
        if !task_needs_tool_name_migration(&task.required_tools, &task.granted_tools) {
            return false;
        }
        self.migrate_task_tool_names_by_id(&task.id)
    }

    fn migrate_task_tool_names_by_id(&self, id: &str) -> bool {
        if self.ensure_coordination_task_write_permitted().is_err() {
            return false;
        }
        match self
            .stores()
            .task_documents()
            .migrate_task_tool_names(id, TOOL_NAMES_MIGRATION_ACTOR)
        {
            Ok(changed) => {
                if changed {
                    tracing::info!(
                        target: "orbit.core.tool_names",
                        task_id = id,
                        "rewrote the task's stored pre-family tool names to family actions"
                    );
                }
                changed
            }
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    task_id = id,
                    %error,
                    "skipped rewriting the task's stored pre-family tool names"
                );
                false
            }
        }
    }

    /// Rewrite every task in this workspace whose stored `required_tools` or
    /// `granted_tools` names a pre-family operation. Returns how many changed.
    pub(crate) fn migrate_stored_task_tool_names(&self) -> Result<usize, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let store = self.stores().tasks();
        let ids = match store.registered_task_bundle_ids()? {
            Some(ids) => ids,
            None => store
                .list_tasks()?
                .into_iter()
                .map(|task| task.id)
                .collect(),
        };
        let mut migrated = 0;
        for batch in ids.chunks(TASK_ENVELOPE_BATCH) {
            let batch = batch.iter().cloned().collect();
            // One unreadable batch never stops the rest converging, nor
            // makes every later open rescan the store.
            let envelopes = match store.task_envelopes_for_ids(&batch) {
                Ok(envelopes) => envelopes,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.tool_names",
                        %error,
                        "skipped a batch of tasks while rewriting pre-family tool names"
                    );
                    continue;
                }
            };
            for envelope in envelopes {
                if task_needs_tool_name_migration(&envelope.required_tools, &envelope.granted_tools)
                    && self.migrate_task_tool_names_by_id(&envelope.id)
                {
                    migrated += 1;
                }
            }
        }
        Ok(migrated)
    }

    /// Rewrite `permissions.orbit_tools` in each installed plugin manifest
    /// that names a pre-family operation, and record the rewritten digest.
    /// Only a manifest whose bytes still carry the digest recorded at install
    /// is touched: the rewrite names the same operations, so it keeps the
    /// operator's consent, while an unconsented edit stays refused.
    pub(crate) fn migrate_plugin_manifest_tool_names(
        &self,
    ) -> Result<Vec<ToolNameRewrite>, OrbitError> {
        let global_root = self.global_root();
        let mut rewrites = Vec::new();
        for mut installed in self.stores().plugins().list_plugins()? {
            if crate::runtime::plugin::grants::verify_install_path(&global_root, &installed)
                .is_err()
            {
                continue;
            }
            let path =
                Path::new(&installed.install_path).join(orbit_types::plugin::MANIFEST_FILE_NAME);
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            if orbit_tools::plugin::manifest_digest(raw.as_bytes()) != installed.manifest_digest {
                continue;
            }
            let Ok(manifest) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
                continue;
            };
            let Some(before) = manifest_orbit_tools(&manifest) else {
                continue;
            };
            let Some(after) = family_action_tool_names(&before) else {
                continue;
            };
            let healed = rewrite_yaml_list_tool_names(&raw, "orbit_tools");
            let healed_manifest = serde_yaml::from_str::<serde_yaml::Value>(&healed).ok();
            let mut expected = manifest.clone();
            set_manifest_orbit_tools(&mut expected, &after);
            let rewritten_tools = healed_manifest.as_ref().and_then(manifest_orbit_tools);
            let mut healed_rest = healed_manifest.clone();
            if let Some(value) = healed_rest.as_mut() {
                set_manifest_orbit_tools(value, &after);
            }
            if rewritten_tools
                .map(orbit_types::task::normalize_required_tools)
                .as_ref()
                != Some(&after)
                || healed_rest.as_ref() != Some(&expected)
            {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    plugin = installed.name.as_str(),
                    "plugin manifest names pre-family tools in a layout that cannot be \
                     rewritten in place"
                );
                continue;
            }
            atomic_write_text(&path, &healed).map_err(|error| {
                OrbitError::io_with_context(&error, format!("rewrite {}: {error}", path.display()))
            })?;
            installed.manifest_digest = orbit_tools::plugin::manifest_digest(healed.as_bytes());
            self.stores().plugins().upsert_plugin(&installed)?;
            rewrites.push(ToolNameRewrite {
                kind: "plugin",
                name: installed.name.clone(),
                path,
                before,
                after,
            });
        }
        record_tool_name_rewrites(&global_root.join("state"), &rewrites);
        Ok(rewrites)
    }

    /// The upgrade pass, once per root: on the first writable open after an
    /// upgrade, rewrite pre-family tool names in the global root's plugin
    /// manifests, and in this workspace's auto-task definitions and tasks.
    /// A root whose pass fails keeps no marker and retries on the next open;
    /// the failure never fails the open, since every reader still accepts the
    /// old names.
    pub(crate) fn migrate_pre_family_tool_names_on_open(&self) {
        let global_state = self.global_root().join("state");
        run_once(&global_state, "plugin manifests", || {
            self.migrate_plugin_manifest_tool_names().map(|_| ())
        });
        let workspace_state = self.paths().state_dir.clone();
        run_once(&workspace_state, "workspace definitions and tasks", || {
            self.heal_local_auto_task_tool_names()?;
            if self.ensure_coordination_task_write_permitted().is_ok() {
                self.migrate_stored_task_tool_names()?;
            }
            Ok(())
        });
    }
}

fn run_once(state_dir: &Path, scope: &str, pass: impl FnOnce() -> Result<(), OrbitError>) {
    let marker = state_dir.join(TOOL_NAME_MIGRATION_MARKER);
    if marker.exists() {
        return;
    }
    let result = pass().and_then(|()| {
        // A store with no state directory yet has nothing to migrate; the
        // open pass never creates (or follows a redirected) state root.
        if !std::fs::symlink_metadata(state_dir).is_ok_and(|meta| meta.is_dir()) {
            return Ok(());
        }
        atomic_write_text(&marker, &format!("{}\n", Utc::now().to_rfc3339()))
            .map_err(OrbitError::from)
    });
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.tool_names",
            scope,
            %error,
            "pre-family tool name migration did not complete; it retries on the next open"
        );
    }
}

fn manifest_orbit_tools(manifest: &serde_yaml::Value) -> Option<Vec<String>> {
    manifest
        .get("spec")?
        .get("permissions")?
        .get("orbit_tools")?
        .as_sequence()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool.as_str().map(str::to_string))
                .collect()
        })
}

fn set_manifest_orbit_tools(manifest: &mut serde_yaml::Value, tools: &[String]) {
    if let Some(slot) = manifest
        .get_mut("spec")
        .and_then(|spec| spec.get_mut("permissions"))
        .and_then(|permissions| permissions.get_mut("orbit_tools"))
    {
        *slot = serde_yaml::Value::Sequence(
            tools
                .iter()
                .map(|tool| serde_yaml::Value::String(tool.clone()))
                .collect(),
        );
    }
}
