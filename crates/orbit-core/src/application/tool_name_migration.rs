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
//! - installed plugin manifests' `permissions.orbit_tools`, rewritten only
//!   when the bytes are still the manifest this host recorded at install.
//!   The rewritten digest is recorded with that rewrite. A failed record
//!   restores the consented bytes, unless a competing pass already recorded
//!   this rewrite, in which case the healed file stays. The read, rewrite,
//!   record, rollback and completion marker share one advisory lock. A file
//!   left as that rewrite is finished on the next pass by recording the
//!   digest it already has.
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
use orbit_types::tool::{family_action_tool_names, is_tool_family_operation, pre_family_tool_name};
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
    rewrite_yaml_keyed_list(raw, key, rewrite_line_tool_names)
}

/// The inverse of [`rewrite_yaml_list_tool_names`] for the same list: family
/// actions standing as list items become the operation names they were stored
/// as. Other bytes stay put, so the result matches the consented manifest
/// when `raw` is that function's rewrite of it.
fn reverse_yaml_list_tool_names(raw: &str, key: &str) -> String {
    rewrite_yaml_keyed_list(raw, key, reverse_line_tool_names)
}

fn rewrite_yaml_keyed_list(raw: &str, key: &str, rewrite_line: fn(&str) -> String) -> String {
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
            rewritten.push_str(&rewrite_line(line));
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

/// Replace each family action on `line` that stands as a whole list item
/// with the operation name it was stored as. Boundaries match
/// [`rewrite_line_tool_names`].
fn reverse_line_tool_names(line: &str) -> String {
    let is_name_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.';
    let is_action_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut rewritten = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(is_name_char) {
        let (before, tail) = rest.split_at(start);
        let name_end = tail.find(|c: char| !is_name_char(c)).unwrap_or(tail.len());
        let mut end = name_end;
        if tail[name_end..].starts_with(':') {
            let action = &tail[name_end + 1..];
            let action_len = action
                .find(|c: char| !is_action_char(c))
                .unwrap_or(action.len());
            if action_len > 0 {
                end = name_end + 1 + action_len;
            }
        }
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
        if opened && closed {
            rewritten.push_str(pre_family_tool_name(token));
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
    heal_auto_task_catalog_with_fork_policy(orbit_root, write, false)
}

/// Rewrite pre-family names during workspace sync while preserving edits to a
/// tracked definition. Upgrade and load paths use `heal_auto_task_catalog`
/// and still migrate every stored definition, including forks.
pub(crate) fn heal_workspace_sync_auto_task_catalog(
    orbit_root: &Path,
    write: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    heal_auto_task_catalog_with_fork_policy(orbit_root, write, true)
}

fn heal_auto_task_catalog_with_fork_policy(
    orbit_root: &Path,
    write: bool,
    preserve_managed_forks: bool,
) -> Result<Vec<ToolNameRewrite>, OrbitError> {
    let dir = auto_tasks_dir(orbit_root);
    match fs::symlink_metadata(&dir) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        _ => return Ok(Vec::new()),
    }
    let manifest = if preserve_managed_forks {
        load_managed_asset_manifest(
            &dir.join(MANAGED_ASSET_MANIFEST_FILE),
            AUTO_TASK_ASSET_KIND,
            ManagedAssetLayout::YamlStem,
        )?
    } else {
        None
    };
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
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        let is_modified_managed_asset = manifest
            .as_ref()
            .and_then(|manifest| manifest.assets.get(name))
            .is_some_and(|expected_digest| {
                fs::read(&path)
                    .ok()
                    .is_some_and(|raw| sha256_hex(&raw) != *expected_digest)
            });
        if is_modified_managed_asset {
            continue;
        }
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
    /// is rewritten: the rewrite names the same operations, so it keeps the
    /// operator's consent, while an unconsented edit stays refused.
    ///
    /// The digest is recorded after the file rewrite. If that record fails
    /// and the row still names the bytes this pass read, those bytes are put
    /// back and this pass returns the error, so the upgrade retries. If the
    /// row already names this rewrite, a competing pass finished it and the
    /// healed file is kept. A file already holding that rewrite — the record
    /// failed and the restore did not land — has its digest recorded on the
    /// next pass without another edit. The caller holds the pass's advisory
    /// lock across this work and the completion marker.
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
            let file_digest = orbit_tools::plugin::manifest_digest(raw.as_bytes());
            if file_digest != installed.manifest_digest {
                // The file is already the consented rewrite and only the
                // stored digest is behind (the record failed after the
                // rewrite, and restoring the old bytes did not land).
                if let Some(rewrite) = consented_plugin_tool_rewrite(
                    &path,
                    &installed.name,
                    &raw,
                    &installed.manifest_digest,
                ) {
                    installed.manifest_digest = file_digest;
                    self.stores().plugins().upsert_plugin(&installed)?;
                    rewrites.push(rewrite);
                    continue;
                }
                // Another opener may have recorded the rewritten digest
                // before this opener's failed upsert restored the old bytes.
                // Reapply only when the stored digest proves this exact
                // rewrite is the one already recorded.
                if let Some((healed, rewrite)) =
                    plugin_manifest_tool_rewrite(&path, &installed.name, &raw)
                    && orbit_tools::plugin::manifest_digest(healed.as_bytes())
                        == installed.manifest_digest
                {
                    atomic_write_text(&path, &healed).map_err(|error| {
                        OrbitError::io_with_context(
                            &error,
                            format!("rewrite {}: {error}", path.display()),
                        )
                    })?;
                    rewrites.push(rewrite);
                }
                continue;
            }
            let Some((healed, rewrite)) =
                plugin_manifest_tool_rewrite(&path, &installed.name, &raw)
            else {
                continue;
            };
            let healed_digest = orbit_tools::plugin::manifest_digest(healed.as_bytes());
            let consented_digest = installed.manifest_digest.clone();
            // Before this pass publishes, so a competing opener still reads
            // the consented bytes and can finish the rewrite itself.
            #[cfg(any(test, feature = "test-support"))]
            plugin_manifest_publish_hook::before_publish();
            atomic_write_text(&path, &healed).map_err(|error| {
                OrbitError::io_with_context(&error, format!("rewrite {}: {error}", path.display()))
            })?;
            installed.manifest_digest = healed_digest.clone();
            #[cfg(any(test, feature = "test-support"))]
            let recorded = if plugin_manifest_publish_hook::take_digest_upsert_failure() {
                Err(OrbitError::Store(
                    "injected plugin digest write failure".to_string(),
                ))
            } else {
                self.stores().plugins().upsert_plugin(&installed)
            };
            #[cfg(not(any(test, feature = "test-support")))]
            let recorded = self.stores().plugins().upsert_plugin(&installed);
            if let Err(error) = recorded {
                // The manifest and the stored digest have to stay a pair.
                // Restoring the consented bytes leaves the plugin loadable
                // when the row is still those bytes. A competing pass that
                // already recorded this rewrite must keep the healed file:
                // the completion marker would otherwise suppress the repair.
                self.settle_failed_plugin_digest_record(FailedPluginDigestRecord {
                    path: &path,
                    consented: &raw,
                    healed: &healed,
                    plugin_name: &installed.name,
                    healed_digest: &healed_digest,
                    consented_digest: &consented_digest,
                    error,
                })?;
                rewrites.push(rewrite);
                continue;
            }
            rewrites.push(rewrite);
        }
        record_tool_name_rewrites(&global_root.join("state"), &rewrites);
        Ok(rewrites)
    }

    /// Decide the file side of a digest record that did not land.
    ///
    /// `Ok` means a competing pass already recorded this exact rewrite, and
    /// the healed bytes are the manifest on disk. `Err` stops the pass: the
    /// consented bytes are back when the row still names them, and the file
    /// is left alone when the row moved somewhere else or could not be read.
    fn settle_failed_plugin_digest_record(
        &self,
        record: FailedPluginDigestRecord<'_>,
    ) -> Result<(), OrbitError> {
        let FailedPluginDigestRecord {
            path,
            consented,
            healed,
            plugin_name,
            healed_digest,
            consented_digest,
            error,
        } = record;
        let recorded = match self.stores().plugins().get_plugin(plugin_name) {
            Ok(plugin) => plugin.map(|plugin| plugin.manifest_digest),
            Err(read_error) => {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    plugin = plugin_name,
                    %error,
                    %read_error,
                    "left the rewritten plugin manifest in place because its stored digest could not be re-read"
                );
                return Err(error);
            }
        };
        if recorded.as_deref() == Some(healed_digest) {
            let on_disk_matches = fs::read(path)
                .is_ok_and(|bytes| orbit_tools::plugin::manifest_digest(&bytes) == healed_digest);
            if !on_disk_matches {
                atomic_write_text(path, healed).map_err(|io_error| {
                    OrbitError::io_with_context(
                        &io_error,
                        format!("keep rewritten {}: {io_error}", path.display()),
                    )
                })?;
            }
            tracing::debug!(
                target: "orbit.core.tool_names",
                plugin = plugin_name,
                "kept a plugin manifest rewrite a competing pass already recorded"
            );
            return Ok(());
        }
        if recorded.as_deref() == Some(consented_digest) {
            if let Err(restore) = atomic_write_text(path, consented) {
                tracing::warn!(
                    target: "orbit.core.tool_names",
                    plugin = plugin_name,
                    %error,
                    %restore,
                    "plugin manifest rewrite could not be restored after its digest was not recorded"
                );
            }
            return Err(error);
        }
        tracing::warn!(
            target: "orbit.core.tool_names",
            plugin = plugin_name,
            %error,
            "left the rewritten plugin manifest in place because its stored digest changed"
        );
        Err(error)
    }

    /// The upgrade pass, once per root: on the first writable open after an
    /// upgrade, rewrite pre-family tool names in the global root's plugin
    /// manifests, and in this workspace's auto-task definitions and tasks.
    /// A root whose pass fails keeps no marker and retries on the next open;
    /// the failure never fails the open, since every reader still accepts the
    /// old names.
    pub(crate) fn migrate_pre_family_tool_names_on_open(&self) {
        let global_state = self.global_root().join("state");
        run_plugin_manifest_migration_once(&global_state, || {
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

/// The file and the row a failed plugin-manifest digest record has to reconcile.
struct FailedPluginDigestRecord<'a> {
    path: &'a Path,
    consented: &'a str,
    healed: &'a str,
    plugin_name: &'a str,
    healed_digest: &'a str,
    consented_digest: &'a str,
    error: OrbitError,
}

/// One plugin-manifest upgrade pass, serialized with its completion marker.
///
/// The lock covers the marker check, the rewrite, the digest record, a
/// rollback and the marker publication. Another process cannot finish the
/// pass, or publish the marker, between this pass's read and its rollback.
/// The state directory is not created here: a root without one still has
/// nothing durable to mark, matching [`run_once`].
fn run_plugin_manifest_migration_once(
    state_dir: &Path,
    pass: impl FnOnce() -> Result<(), OrbitError>,
) {
    const SCOPE: &str = "plugin manifests";
    if !std::fs::symlink_metadata(state_dir).is_ok_and(|meta| meta.is_dir()) {
        run_once(state_dir, SCOPE, pass);
        return;
    }
    let marker = state_dir.join(TOOL_NAME_MIGRATION_MARKER);
    // Published under this lock, so a marker that is already present is a
    // finished pair. The check is repeated inside the lock.
    if marker.exists() {
        return;
    }
    if let Err(error) = with_exclusive_file_lock(
        &marker,
        "plugin manifest tool-name migration",
        || -> std::io::Result<()> {
            run_once(state_dir, SCOPE, pass);
            Ok(())
        },
    ) {
        tracing::warn!(
            target: "orbit.core.tool_names",
            scope = SCOPE,
            %error,
            "pre-family plugin manifest migration could not take its lock; it retries on the next open"
        );
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

/// `raw` is the in-place family-action rewrite of the manifest whose digest
/// is `stored_digest`. Any other edit is `None`, so an unconsented change
/// stays refused.
fn consented_plugin_tool_rewrite(
    path: &Path,
    name: &str,
    raw: &str,
    stored_digest: &str,
) -> Option<ToolNameRewrite> {
    let reversed = reverse_yaml_list_tool_names(raw, "orbit_tools");
    if reversed == raw || orbit_tools::plugin::manifest_digest(reversed.as_bytes()) != stored_digest
    {
        return None;
    }
    let (healed, rewrite) = plugin_manifest_tool_rewrite(path, name, &reversed)?;
    (healed == raw).then_some(rewrite)
}

/// Return the consent-preserving in-place rewrite of a plugin manifest when
/// the complete parsed manifest changes only by mapping its tool list.
fn plugin_manifest_tool_rewrite(
    path: &Path,
    name: &str,
    raw: &str,
) -> Option<(String, ToolNameRewrite)> {
    let manifest = serde_yaml::from_str::<serde_yaml::Value>(raw).ok()?;
    let before = manifest_orbit_tools(&manifest)?;
    let after = family_action_tool_names(&before)?;
    let healed = rewrite_yaml_list_tool_names(raw, "orbit_tools");
    let healed_manifest = serde_yaml::from_str::<serde_yaml::Value>(&healed).ok();
    let mut expected = manifest;
    set_manifest_orbit_tools(&mut expected, &after);
    let rewritten_tools = healed_manifest.as_ref().and_then(manifest_orbit_tools);
    let mut healed_rest = healed_manifest;
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
            plugin = name,
            "plugin manifest names pre-family tools in a layout that cannot be rewritten in place"
        );
        return None;
    }
    Some((
        healed,
        ToolNameRewrite {
            kind: "plugin",
            name: name.to_string(),
            path: path.to_path_buf(),
            before,
            after,
        },
    ))
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

/// Same-thread rendezvous for the plugin-manifest publish race.
///
/// A nested runtime open on this thread re-enters the migration lock, which
/// is what lets a test finish the competing pass after this pass has read
/// the consented manifest and before it publishes. Cross-process opens wait
/// on the lock instead.
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod plugin_manifest_publish_hook {
    use std::cell::{Cell, RefCell};

    type PublishHook = Box<dyn FnOnce()>;

    thread_local! {
        static HOOK: RefCell<Option<PublishHook>> = const { RefCell::new(None) };
        static FAIL_DIGEST_UPSERT: Cell<bool> = const { Cell::new(false) };
    }

    struct RestoreHook(Option<PublishHook>);

    impl Drop for RestoreHook {
        fn drop(&mut self) {
            HOOK.with(|slot| {
                *slot.borrow_mut() = self.0.take();
            });
            FAIL_DIGEST_UPSERT.with(|flag| flag.set(false));
        }
    }

    /// Run `operation` with a one-shot callback immediately before the first
    /// consented plugin-manifest publish of a tool-name migration on this
    /// thread.
    #[cfg(feature = "test-support")]
    pub fn with_plugin_manifest_publish_hook<T>(
        hook: impl FnOnce() + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        let previous = HOOK.with(|slot| slot.replace(Some(Box::new(hook))));
        let _restore = RestoreHook(previous);
        operation()
    }

    /// Fail the next plugin-manifest digest record on this thread as a store
    /// error, without writing the row.
    ///
    /// A digest-change trigger cannot inject this once a competing pass has
    /// already stored the same digest: the update no longer changes the column.
    #[cfg(feature = "test-support")]
    pub fn fail_next_plugin_digest_upsert() {
        FAIL_DIGEST_UPSERT.with(|flag| flag.set(true));
    }

    pub(super) fn before_publish() {
        let hook = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    pub(super) fn take_digest_upsert_failure() -> bool {
        FAIL_DIGEST_UPSERT.with(|flag| flag.replace(false))
    }
}
