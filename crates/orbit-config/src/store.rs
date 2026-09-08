//! Surgical, comment-preserving `config.toml` reads/writes for `orbit config`.
//!
//! [`ConfigStore`] wraps a single `config.toml` file as a `toml_edit::DocumentMut`
//! so `orbit config set` can edit one key without disturbing any other part of
//! the file's formatting or hand-written comments. Validation goes through the
//! same single-document admission pipeline used after runtime layers have been
//! merged, so a `set` cannot produce a malformed value for its target file.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::security::redaction::redact_home_dir;

use crate::persistence::PersistenceConfig;
use crate::registry::{self, ConfigSnapshot};
use crate::resolved::ResolvedConfig;

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
    scope: ConfigScope,
    path: PathBuf,
    doc: DocumentMut,
}

impl ConfigStore {
    /// Open `path` for the given scope, reading its current content if it
    /// exists on disk. A missing file is not an error: it opens as an empty
    /// document, matching how [`ResolvedConfig::load`] treats a missing
    /// config (every setting falls back to its default).
    pub fn open(scope: ConfigScope, path: impl Into<PathBuf>) -> Result<Self, OrbitError> {
        let path = path.into();
        let content = read_optional(&path)?;
        Self::from_content(scope, path, &content)
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
        if path.exists() {
            return Self::open(ConfigScope::Workspace, path);
        }
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
        Self::from_content(ConfigScope::Workspace, path, &content)
    }

    fn from_content(scope: ConfigScope, path: PathBuf, content: &str) -> Result<Self, OrbitError> {
        let doc = content.parse::<DocumentMut>().map_err(|err| {
            OrbitError::InvalidInput(format!(
                "invalid TOML in '{}': {err}",
                redact_home_dir(&path.display().to_string())
            ))
        })?;
        Ok(Self { scope, path, doc })
    }

    /// Which physical file this store is bound to.
    pub fn scope(&self) -> ConfigScope {
        self.scope
    }

    /// The bound file path, whether or not it exists yet.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The fully resolved (defaulted) view of this document, as if it were
    /// loaded as the effective `config.toml`. Used by both `orbit config
    /// show` and `orbit config get` so they report identical values for the
    /// same scope.
    pub fn snapshot(&self) -> Result<ConfigSnapshot, OrbitError> {
        Ok(self.resolved()?.snapshot)
    }

    fn resolved(&self) -> Result<ResolvedConfig, OrbitError> {
        // Persistence paths are derived from the two data roots, not from
        // the config document, and are irrelevant to key validation here.
        let persistence =
            PersistenceConfig::default_for_data_root(self.path.parent().unwrap_or(&self.path));
        ResolvedConfig::from_raw_str(&self.doc.to_string(), &self.path, persistence)
    }

    /// Look up the effective value of a single admitted key.
    pub fn effective_value(&self, key: &str) -> Result<JsonValue, OrbitError> {
        registry::admit_config_key(key)?;
        let resolved = self.resolved()?;
        if let Some(value) = resolved.snapshot.value_for(key) {
            return Ok(value);
        }
        Ok(crew_field_value(&resolved, key)?.unwrap_or(JsonValue::Null))
    }

    /// Set `key` to the TOML-literal-or-string parse of `raw_value`,
    /// mutating the in-memory document only. Callers must call
    /// [`Self::validate`] and then [`Self::save`] afterward — `set_value`
    /// never touches disk.
    pub fn set_value(&mut self, key: &str, raw_value: &str) -> Result<(), OrbitError> {
        registry::admit_config_key(key)?;
        self.set_document_value(key, raw_value)
    }

    /// Set a value in a configuration section that is parsed by a subsystem
    /// outside the runtime-key registry.
    ///
    /// The registry intentionally admits only runtime-owned keys, while the
    /// same `config.toml` also contains independently owned sections such as
    /// `[docs]`. Those owners still use this method so their edits preserve
    /// comments and share [`Self::save`]'s atomic persistence.
    pub fn set_document_value(&mut self, key: &str, raw_value: &str) -> Result<(), OrbitError> {
        if key.split('.').any(str::is_empty) {
            return Err(OrbitError::InvalidInput(
                "config key must contain non-empty dot-separated segments".to_string(),
            ));
        }
        let value = parse_value_literal(raw_value);
        let segments: Vec<&str> = key.split('.').collect();
        // The non-empty segment check above makes `split_last` always `Some`;
        // still handle it as an error rather than `expect()` because this is
        // reachable from user input, not a purely local invariant.
        let (last, ancestors) = segments.split_last().ok_or_else(|| {
            OrbitError::InvalidInput(format!("config key '{key}' must not be empty"))
        })?;

        let mut table: &mut dyn TableLike = self.doc.as_table_mut();
        for segment in ancestors {
            let item = table
                .entry(segment)
                .or_insert_with(|| Item::Table(Table::new()));
            table = item.as_table_like_mut().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "cannot set '{key}': '{segment}' along its path is already a non-table value \
                     in '{}'",
                    redact_home_dir(&self.path.display().to_string())
                ))
            })?;
        }
        // Prefer mutating an existing key's `Item` in place over
        // `Table::insert`, which always constructs a brand-new `Key` node:
        // that would silently drop any full-line comment attached to the
        // existing key (its "decor") even though we're only replacing the
        // value.
        match table.get_mut(last) {
            Some(existing) => *existing = Item::Value(value),
            None => {
                table.insert(last, Item::Value(value));
            }
        }
        Ok(())
    }

    /// Run the in-memory document through the exact same
    /// `RawRuntimeConfig` → [`ResolvedConfig`] validation pipeline as
    /// [`ResolvedConfig::load`], without writing anything.
    pub fn validate(&self) -> Result<(), OrbitError> {
        self.snapshot().map(|_| ())
    }

    /// Atomically write the current in-memory document to `self.path`
    /// (temp file + rename, via `orbit_common::fs::io::atomic_write_text`).
    /// Callers should call [`Self::validate`] first: `save` does not
    /// validate on its own.
    pub fn save(&self) -> Result<(), OrbitError> {
        atomic_write_text(&self.path, &self.doc.to_string()).map_err(|err| {
            OrbitError::Io(format!(
                "failed to write config '{}': {err}",
                redact_home_dir(&self.path.display().to_string())
            ))
        })
    }
}

fn crew_field_value(resolved: &ResolvedConfig, key: &str) -> Result<Option<JsonValue>, OrbitError> {
    let Some(parsed) = registry::parse_crew_field_key(key)? else {
        return Ok(None);
    };
    let Some(crew) = resolved.crews.get(parsed.name) else {
        return Ok(Some(JsonValue::Null));
    };
    Ok(Some(match parsed.field {
        "model" => serde_json::json!(crew.assignment.model),
        "provider" => serde_json::json!(crew.assignment.provider),
        "effort" => serde_json::json!(crew.assignment.effort),
        "description" => serde_json::json!(crew.description),
        "tags" => serde_json::json!(crew.tags),
        _ => JsonValue::Null,
    }))
}

fn read_optional(path: &Path) -> Result<String, OrbitError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(OrbitError::Io(format!(
            "failed to read config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))),
    }
}

/// Parse `raw` as a TOML literal (bool/int/float/array/inline-table/etc),
/// falling back to a plain string if it doesn't parse as one. No `--type`
/// flag: this is the entire type-inference rule for `orbit config set`.
///
/// `toml_edit::Value` (like `toml::Value`) has no public "parse a bare
/// value" entry point — both crates' string parsers expect a full
/// `key = value` document, so `"10000".parse::<toml_edit::Value>()` is not
/// a thing. Instead, wrap `raw` as the value of a throwaway key in a
/// synthetic one-line document, parse *that*, and pull the value back out.
/// Any extra keys `raw` might smuggle in (e.g. `1\nother = "x"`) are parsed
/// but never read, so this can't be used to inject unrelated keys — worst
/// case a crafted `raw` just fails to parse and falls back to a string.
fn parse_value_literal(raw: &str) -> toml_edit::Value {
    const SCRATCH_KEY: &str = "_orbit_config_set_value";
    let synthetic = format!("{SCRATCH_KEY} = {raw}");
    synthetic
        .parse::<DocumentMut>()
        .ok()
        .and_then(|doc| doc.get(SCRATCH_KEY).and_then(Item::as_value).cloned())
        .unwrap_or_else(|| toml_edit::Value::from(raw))
}
