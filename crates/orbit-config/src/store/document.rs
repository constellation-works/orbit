//! Comment-preserving TOML document mutations.

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use toml_edit::{DocumentMut, Item, Table, TableLike};

use super::persistence::{ConfigScope, ConfigStore};
use crate::registry::{self, ConfigSnapshot};

impl ConfigStore {
    /// The document as staged in memory, edits included.
    pub(crate) fn staged_text(&self) -> String {
        self.doc.to_string()
    }

    /// Check if `key` is explicitly defined in this store's TOML document.
    pub fn is_key_set(&self, key: &str) -> bool {
        let mut item: &toml_edit::Item = self.doc.as_item();
        for segment in key.split('.') {
            let Some(table) = item.as_table_like() else {
                return false;
            };
            let Some(next) = table.get(segment) else {
                return false;
            };
            item = next;
        }
        !matches!(item, toml_edit::Item::None)
    }

    /// Seed an absent key with an inline explanation, preserving existing values.
    pub(crate) fn seed_missing_value(
        &mut self,
        key: &str,
        raw_value: &str,
        comment: &str,
    ) -> Result<bool, OrbitError> {
        if self.is_key_set(key) {
            return Ok(false);
        }
        self.set_value(key, raw_value)?;
        let mut item = self.doc.as_item_mut();
        for segment in key.split('.') {
            item = item.get_mut(segment).ok_or_else(|| {
                OrbitError::InvalidInput(format!("seeded config key '{key}' is missing"))
            })?;
        }
        if let Some(value) = item.as_value_mut() {
            value.decor_mut().set_suffix(format!(" # {comment}"));
        }
        Ok(true)
    }

    /// Names of the `[crews.<name>]` tables this document defines, sorted.
    pub fn crew_table_names(&self) -> Vec<String> {
        let mut names = self
            .doc
            .get("crews")
            .and_then(Item::as_table_like)
            .map(|crews| {
                crews
                    .iter()
                    .filter(|(_, item)| item.is_table_like())
                    .map(|(name, _)| name.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Set `key` to the TOML-literal-or-string parse of `raw_value` in memory,
    /// checking a fixed registry key's raw type before staging the edit.
    /// Callers must call
    /// [`Self::validate`] and then [`Self::save`] afterward — `set_value`
    /// never touches disk.
    pub fn set_value(&mut self, key: &str, raw_value: &str) -> Result<(), OrbitError> {
        registry::admit_settable_config_key(key)?;
        self.reject_global_only_key(key)?;
        ConfigSnapshot::validate_supplied_value(key, &parse_value_literal(raw_value))?;
        self.set_document_value(key, raw_value)
    }

    /// Refuse a global-only key on a workspace-bound store, naming the flag
    /// that targets the right file. Checked before any document mutation so a
    /// refused `set` leaves the workspace file untouched.
    fn reject_global_only_key(&self, key: &str) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Workspace || !registry::is_global_only_key(key) {
            return Ok(());
        }
        let global_path = self
            .global_config_path
            .as_ref()
            .map(|path| format!(" in '{}'", redact_home_dir(&path.display().to_string())))
            .unwrap_or_default();
        Err(OrbitError::InvalidInput(format!(
            "config key '{key}' belongs to the global config only; rerun with --global to edit \
             it{global_path}"
        )))
    }

    /// Record this machine's identity in the global `config.toml`.
    ///
    /// `orbit init` is the only writer of `machine.id` and `machine.task_prefix`
    /// — [`Self::set_value`] refuses both — so creating an identity goes
    /// through this seam rather than the operator-facing one. Refuses a
    /// workspace-bound store outright: the table is global-only.
    ///
    /// The staged document is reparsed before the caller can save it, so an
    /// identity that would not round-trip fails here rather than producing an
    /// unreadable `[machine]` table. Deliberately narrower than
    /// [`Self::validate`]: an unrelated admission problem elsewhere in an
    /// operator's config must not stop `orbit init` from recording who this
    /// machine is.
    pub fn set_machine_identity(
        &mut self,
        id: &str,
        name: &str,
        task_prefix: &str,
    ) -> Result<(), OrbitError> {
        if self.scope != ConfigScope::Global {
            return Err(OrbitError::InvalidInput(
                "machine identity is written only to the global config.toml".to_string(),
            ));
        }
        let fields = [
            ("machine.id", id),
            ("machine.name", name),
            ("machine.task_prefix", task_prefix),
        ];
        for (key, value) in fields {
            // Render as a TOML basic string rather than letting the literal
            // parser reinterpret an identity that happens to look like a
            // number, array, or inline table.
            self.set_document_value(key, &toml_edit::Value::from(value).to_string())?;
        }
        let staged: toml::Value = toml::from_str(&self.doc.to_string()).map_err(|err| {
            OrbitError::InvalidInput(format!("staged machine identity is not valid TOML: {err}"))
        })?;
        for (key, value) in fields {
            let read_back = key
                .split('.')
                .try_fold(&staged, |table, segment| table.get(segment))
                .and_then(toml::Value::as_str);
            if read_back != Some(value) {
                return Err(OrbitError::InvalidInput(format!(
                    "staged machine identity does not round-trip: '{key}' reads back as \
                     {read_back:?}; refusing to write a corrupt [machine] table"
                )));
            }
        }
        Ok(())
    }

    /// Set a value in a configuration section that is parsed by a subsystem
    /// outside the runtime-key registry.
    ///
    /// The registry intentionally admits only runtime-owned keys, while the
    /// same `config.toml` can also contain independently owned sections. Those
    /// owners use this method so their edits preserve comments and share
    /// [`Self::save`]'s atomic persistence.
    pub fn set_document_value(&mut self, key: &str, raw_value: &str) -> Result<(), OrbitError> {
        if key.split('.').any(str::is_empty) {
            return Err(OrbitError::InvalidInput(
                "config key must contain non-empty dot-separated segments".to_string(),
            ));
        }
        let value = parse_value_literal(raw_value);
        // Loading clamps legacy persisted run windows so the CLI remains
        // usable, but an explicit write must not persist an unsafe window.
        // Check the parsed literal (including hexadecimal TOML integers)
        // before mutating the document, for every config-store writer.
        if key == "retention.runs_days"
            && let Some(days) = value.as_integer()
            && days < 30
        {
            return Err(OrbitError::InvalidInput(format!(
                "{key} has invalid value {days}; expected 30..=36500: Reliability Pipeline \
                 echo dedupe and forecast-accuracy per-drain limiter shares require 30 days of run state"
            )));
        }
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

    /// Remove `key` from this document, reporting whether it was present.
    ///
    /// The inverse of [`Self::set_value`] for a dashboard or script that
    /// clears one key so the layer below (or the built-in default) takes over
    /// again. Emptied parent tables are left in place: a `[workflow]` header
    /// with a hand-written comment above it is content an operator wrote, and
    /// an empty table admits exactly like an absent one.
    /// The machine identity keys are the one exception: there is no layer
    /// below this machine's identity to fall back to, and clearing one of its
    /// three keys would leave a partial `[machine]` table that fails closed on
    /// the next load. `machine.worker_*` limits unset to their defaults.
    ///
    /// A removed or deprecated registry key is the other exception.
    /// `orbit config get` and `set` refuse it, and this method still removes
    /// it so `orbit doctor --fix-retired-config-keys` can delete a key the
    /// operator can no longer write.
    pub fn unset_value(&mut self, key: &str) -> Result<bool, OrbitError> {
        if !registry::is_compatibility_config_key(key) {
            registry::admit_settable_config_key(key)?;
            if registry::is_machine_identity_key(key) {
                return Err(OrbitError::InvalidInput(format!(
                    "config key '{key}' cannot be unset: this machine's identity has no layer to \
                     fall back to, and a partial [machine] table does not load. Rename it with \
                     `orbit config set --global machine.name <value>` instead"
                )));
            }
        }
        Ok(self.remove_path(key))
    }

    /// Remove a diagnosed unknown key or whole table from this document.
    ///
    /// Rechecks the staged document so a stale diagnosis cannot remove a known
    /// setting. Literal dots in TOML keys remain separate from path separators.
    pub fn remove_unknown_key(
        &mut self,
        key: &crate::UnknownConfigKey,
    ) -> Result<bool, OrbitError> {
        let document = toml::from_str(&self.doc.to_string())
            .map_err(|error| OrbitError::InvalidInput(format!("invalid staged config: {error}")))?;
        if !crate::unknown_config_keys_present(&document)
            .iter()
            .any(|current| current.segments == key.segments)
        {
            return Ok(false);
        }
        let segments: Vec<_> = key.segments.iter().map(String::as_str).collect();
        Ok(self.remove_segments(&segments))
    }

    /// Remove a whole `[crews.<name>]` table, reporting whether it existed.
    ///
    /// Crew tables are dynamically named, so they are not registry keys and
    /// [`Self::unset_value`] refuses their bare path; deleting a crew still
    /// has to be expressible without listing its fields one by one.
    pub fn remove_crew_table(&mut self, name: &str) -> Result<bool, OrbitError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(OrbitError::InvalidInput(
                "crew config keys require a non-empty crew name".to_string(),
            ));
        }
        if trimmed.contains('.') {
            return Err(OrbitError::InvalidInput(format!(
                "crew name '{trimmed}' must not contain '.'"
            )));
        }
        crate::crew_pools::reject_unpoolable_crew_name(trimmed, "crew config keys")?;
        Ok(self.remove_path(&format!("crews.{trimmed}")))
    }

    /// Drop one dotted path from the document, if every segment along it is a
    /// table and the leaf exists.
    fn remove_path(&mut self, key: &str) -> bool {
        let segments: Vec<&str> = key.split('.').collect();
        self.remove_segments(&segments)
    }

    fn remove_segments(&mut self, segments: &[&str]) -> bool {
        let Some((last, ancestors)) = segments.split_last() else {
            return false;
        };
        let mut table: &mut dyn TableLike = self.doc.as_table_mut();
        for segment in ancestors {
            let Some(next) = table.get_mut(segment).and_then(Item::as_table_like_mut) else {
                return false;
            };
            table = next;
        }
        table.remove(last).is_some()
    }
}

/// Parse `raw` as a TOML literal (bool/int/float/array/inline-table/etc),
/// falling back to a plain string if it doesn't parse as one. No `--type`
/// flag: this is the entire type-inference rule for `orbit config set`.
///
/// Wrap `raw` as the value of a throwaway key in a synthetic one-line
/// document, parse *that*, and pull the value back out. Parsing in
/// document position keeps the value's decor as written, rather than the
/// cleared decor a bare-value parse returns.
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
