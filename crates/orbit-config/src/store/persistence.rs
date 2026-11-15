//! Config store ownership, locking and atomic persistence.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, create_private_dir_all, with_exclusive_file_lock};
use orbit_common::security::redaction::redact_home_dir;
use toml_edit::{DocumentMut, Item};

use crate::ConfigRoots;
use crate::config_io::{config_store_path_error, read_config_file, validated_config_store_path};
use crate::layering::{strip_workspace_refused_keys, validate_staged_workspace_document};
use crate::plugin_enablement::{PLUGIN_ENABLEMENT_TABLE, workspace_config_sets_policy};

/// Which physical `config.toml` file a [`ConfigStore`] is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigScope {
    /// The machine-wide `~/.orbit/config.toml`.
    Global,
    /// The workspace-local `.orbit/config.toml`.
    Workspace,
}

impl ConfigScope {
    /// Stable label used in command output.
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// How to initialize a workspace `config.toml` that doesn't exist yet, for
/// the first `orbit config set` write against it. Fail-closed by default: a
/// bare `set` (without `--global`) must never silently create a workspace and
/// thereby switch sandbox, approval, and environment allowlist values from
/// global policy to built-in workspace defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceInitMode {
    /// The workspace file must already exist; error out with a hint otherwise.
    RequireExisting,
    /// Seed the new workspace file from the current global file's content
    /// (or an empty document if no global file exists either).
    SeedFromGlobal,
    /// Start from an empty TOML document.
    Fresh,
}

/// An in-memory, surgically-editable view of one `config.toml` file, plus
/// the machinery to validate an edit and atomically persist it.
pub struct ConfigStore {
    pub(super) scope: ConfigScope,
    pub(super) path: PathBuf,
    /// The global config path used when this workspace store was opened for
    /// a write, so global-only key refusals can point at the file `--global`
    /// will edit.
    pub(super) global_config_path: Option<PathBuf>,
    pub(super) doc: DocumentMut,
    /// The file's content when this store read it (empty when absent): the
    /// version [`Self::save`] requires to still be on disk before replacing it.
    opened: String,
}

/// Label the config write lock reports in contention diagnostics.
const CONFIG_WRITE_LOCK_LABEL: &str = "config.toml write";

impl ConfigStore {
    /// Open `path` for the given scope, reading its current content if it
    /// exists on disk. A missing file is not an error: it opens as an empty
    /// document, matching how [`ResolvedConfig::load`](crate::ResolvedConfig::load) treats a missing
    /// config (every setting falls back to its default).
    pub fn open(scope: ConfigScope, path: impl Into<PathBuf>) -> Result<Self, OrbitError> {
        let path = path.into();
        let content = read_optional(&path)?;
        Self::from_content(scope, path, &content, content.clone())
    }

    /// Run `op` holding the exclusive write lock of the `config.toml` at
    /// `path`, the lock [`Self::save`] takes.
    ///
    /// A writer opens its store, edits, validates and saves inside `op`, so
    /// the version it validated is the version it replaced, and a concurrent
    /// writer of the same file waits instead of being refused as stale. The
    /// lock is reentrant on one thread; it must not be held across an
    /// `.await`, so async callers run the edit on a blocking thread.
    pub fn with_write_lock<T>(
        path: &Path,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        // Lock the reconstructed, prefix-checked path, as reads do. A missing
        // parent is created first (the save would create it anyway), so every
        // writer of one file agrees on a single lock.
        let target = match validated_config_store_path(path)? {
            Some(target) => target,
            None => {
                let parent = path.parent().ok_or_else(|| {
                    config_store_path_error("config path has no parent directory", path)
                })?;
                create_private_dir_all(parent).map_err(|err| {
                    OrbitError::Io(format!(
                        "failed to create config directory '{}': {err}",
                        redact_home_dir(&parent.display().to_string())
                    ))
                })?;
                validated_config_store_path(path)?.ok_or_else(|| {
                    config_store_path_error("config directory could not be resolved", path)
                })?
            }
        };
        with_exclusive_file_lock(&target, CONFIG_WRITE_LOCK_LABEL, op)
    }

    /// Open the workspace `config.toml` for a `set`, applying the
    /// fail-closed first-write rule: when the workspace file does not yet
    /// exist, `mode` decides whether to seed it from the global file, start
    /// fresh, or refuse.
    pub fn open_for_workspace_set(
        workspace_config_path: impl Into<PathBuf>,
        global_config_path: &Path,
        mode: WorkspaceInitMode,
    ) -> Result<Self, OrbitError> {
        let path = workspace_config_path.into();
        if let (Some(workspace_root), Some(global_root)) =
            (path.parent(), global_config_path.parent())
        {
            let roots = ConfigRoots::new(global_root, workspace_root);
            if !roots.has_workspace_layer() {
                validate_staged_workspace_document(&roots, &path, "")?;
            }
        }
        // A file holding only `[plugin_enablement]` is not a policy layer
        // yet, so the first real setting written into it gets the same
        // fail-closed choice as a missing file; the toggles are carried over.
        if workspace_config_sets_policy(&path) {
            let mut store = Self::open(ConfigScope::Workspace, path)?;
            store.global_config_path = Some(global_config_path.to_path_buf());
            return Ok(store);
        }
        let mut existing = Self::open(ConfigScope::Workspace, path.clone())?;
        let toggles = existing.doc.remove(PLUGIN_ENABLEMENT_TABLE);
        let content = match mode {
            WorkspaceInitMode::RequireExisting => {
                return Err(OrbitError::invalid_input_with_suggestions(
                    format!(
                        "no workspace config exists yet at '{}'; `orbit config set` without \
                         --global refuses to create one implicitly because doing so makes the \
                         security-sensitive sandbox, approval, and environment settings use \
                         workspace values or built-in defaults. Rerun with --seed-from-global to \
                         copy the current global policy explicitly, or --fresh to accept built-in \
                         security defaults and start from an empty file",
                        redact_home_dir(&path.display().to_string())
                    ),
                    Vec::new(),
                ));
            }
            WorkspaceInitMode::SeedFromGlobal => read_optional(global_config_path)?,
            WorkspaceInitMode::Fresh => String::new(),
        };
        let mut store =
            Self::from_content(ConfigScope::Workspace, path, &content, existing.opened)?;
        store.global_config_path = Some(global_config_path.to_path_buf());
        if mode == WorkspaceInitMode::SeedFromGlobal {
            // Global-only keys (`[machine]`, crew env) are rejected in
            // workspace configs. Remove them from
            // the parsed document so all other global settings and their
            // formatting/comments survive seeding.
            strip_workspace_refused_keys(&mut store.doc);
        }
        if let Some(toggles) = toggles {
            store.doc.insert(PLUGIN_ENABLEMENT_TABLE, toggles);
        }
        Ok(store)
    }

    fn from_content(
        scope: ConfigScope,
        path: PathBuf,
        content: &str,
        opened: String,
    ) -> Result<Self, OrbitError> {
        let doc = content.parse::<DocumentMut>().map_err(|err| {
            OrbitError::InvalidInput(format!(
                "invalid TOML in '{}': {err}",
                redact_home_dir(&path.display().to_string())
            ))
        })?;
        Ok(Self {
            scope,
            path,
            global_config_path: None,
            doc,
            opened,
        })
    }

    /// Which physical file this store is bound to.
    pub fn scope(&self) -> ConfigScope {
        self.scope
    }

    /// The bound file path, whether or not it exists yet.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The exact bytes this store was opened from (empty when absent).
    ///
    /// These are the bytes the no-follow config opener returned, so callers
    /// needing the raw document reuse this instead of rereading the path.
    pub fn raw_text(&self) -> &str {
        &self.opened
    }

    /// Whether the file backing this store exists on disk.
    pub fn exists_on_disk(&self) -> bool {
        self.path.exists()
    }

    /// Atomically write the current in-memory document to `self.path`
    /// (temp file + rename, via `orbit_common::fs::io::atomic_write_text`).
    /// Callers should run the validation method appropriate to this store's
    /// scope first: `save` does not validate on its own.
    ///
    /// Under the file's write lock, the file is reread first. When it no
    /// longer holds the content this store was opened from, another writer
    /// saved in between: the save is refused and the file is left as that
    /// writer persisted it, rather than replaced by an edit validated against
    /// an older version. Run the open-to-save sequence inside
    /// [`Self::with_write_lock`] to wait for such a writer instead.
    pub fn save(&mut self) -> Result<(), OrbitError> {
        // Every writer (CLI, doctor, migration, or unrelated edit) uses the
        // spelling older running clients admit. Keep canonical values and
        // comments untouched; only normalize aliases with the same meaning.
        // [ORB-20099] Alias normalization preserves the resolved timing and
        // its source: it never migrates `before_pr` to `before_landing` or
        // `off`, and never materializes a masked global timing into another
        // layer. Only an explicit `orbit config set review.timing
        // before_landing` changes the timing.
        if let Some(value) = self
            .doc
            .get_mut("review")
            .and_then(Item::as_table_like_mut)
            .and_then(|table| table.get_mut("timing"))
            .and_then(Item::as_value_mut)
            && let Some(raw) = value.as_str()
        {
            let timing = crate::operation::ReviewTimingSetting::parse_in_file(raw, &self.path)?;
            if raw != timing.as_str() {
                let mut canonical = toml_edit::Value::from(timing.as_str());
                *canonical.decor_mut() = value.decor().clone();
                *value = canonical;
            }
        }
        let content = self.doc.to_string();
        Self::with_write_lock(&self.path, || {
            if read_optional(&self.path)? != self.opened {
                return Err(OrbitError::Store(format!(
                    "config '{}' changed on disk after this edit read it; nothing was written. \
                     Reapply the edit to the current file",
                    redact_home_dir(&self.path.display().to_string())
                )));
            }
            atomic_write_text(&self.path, &content).map_err(|err| {
                OrbitError::Io(format!(
                    "failed to write config '{}': {err}",
                    redact_home_dir(&self.path.display().to_string())
                ))
            })
        })?;
        self.opened = content;
        Ok(())
    }
}

pub(super) fn read_optional(path: &Path) -> Result<String, OrbitError> {
    read_config_file(path).map(Option::unwrap_or_default)
}
