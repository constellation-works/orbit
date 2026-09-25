use super::*;

/// Every literal a key's resolver accepts, or an empty list when the key is
/// free-form.
///
/// The choices are read from the same constants the resolvers admit against,
/// so an editor that offers them cannot drift from what a write would accept,
/// and a retired choice disappears from both at once.
pub fn config_key_options(key: &str) -> Vec<&'static str> {
    match key {
        "execution.codex.sandbox" => CODEX_PROVIDER_SANDBOX_MODES.to_vec(),
        "execution.codex.approval_policy" => CODEX_APPROVAL_POLICIES.to_vec(),
        "operation.review_policy" => ReviewPolicy::CHOICES.to_vec(),
        _ => Vec::new(),
    }
}

/// Look up one registry key's metadata.
pub fn describe(key: &str) -> Option<&'static ConfigKeyDescriptor> {
    CONFIG_KEY_REGISTRY.iter().find(|entry| entry.key == key)
}

/// Fixed keys retired from the registry that an existing `config.toml` may
/// still carry. Loading warns and ignores each one for one release (see
/// `resolved::warn_compatibility_keys`); `orbit config get`/`set` refuse it
/// with the migration note instead of a did-you-mean, so the operator learns
/// the key is gone rather than misspelled. Delete an entry together with its
/// load warning once the release window has passed.
pub(crate) const REMOVED_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "workflow.pilot_max_complexity",
        "the task pilot applies its assessed complexity as-is; route a tier with \
     workflow.<tier>_complexity_crews or pin `crew` on the task instead",
    ),
    (
        "semantic",
        "semantic search was removed; delete the table and use lexical search",
    ),
    (
        "search.model",
        "search uses SQLite FTS5 and no longer selects a model; delete this key",
    ),
    // Operation mode was removed on 2026-09-21; the `[operation]` table keeps
    // only the review keys. See docs/design/orbit-core/4_decisions.md.
    ("operation.preset", OPERATION_MODE_REMOVED_NOTE),
    ("operation.completion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.preparation", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.preparation_due_seconds",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.promotion", OPERATION_MODE_REMOVED_NOTE),
    ("operation.leaf_ceiling", OPERATION_MODE_REMOVED_NOTE),
    ("operation.recovery", OPERATION_MODE_REMOVED_NOTE),
    (
        "operation.recovery_episodes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    (
        "operation.recovery_minutes_per_task",
        OPERATION_MODE_REMOVED_NOTE,
    ),
    ("operation.delivery_cap", OPERATION_MODE_REMOVED_NOTE),
];

const OPERATION_MODE_REMOVED_NOTE: &str = "operation mode was removed; the [operation] table \
     keeps only review_policy, review_crew, review_reviewer_starts, review_repair_cycles and \
     review_minutes";

/// The migration note for a removed key, or `None` for any other key.
pub(crate) fn removed_key_note(key: &str) -> Option<&'static str> {
    let key = if key.starts_with("semantic.") {
        "semantic"
    } else {
        key
    };
    REMOVED_CONFIG_KEYS
        .iter()
        .find(|(removed, _)| *removed == key)
        .map(|(_, note)| *note)
}

/// Registry keys `orbit config set` refuses to write, with the reason an
/// operator needs. Unlike [`REMOVED_CONFIG_KEYS`] these are live settings —
/// readable by `orbit config get`/`show` and admitted at load — they are
/// simply not the operator's to change after `orbit init` recorded them.
pub(crate) const IMMUTABLE_CONFIG_KEYS: &[(&str, &str)] = &[
    (
        "machine.id",
        "a machine identity is generated once by `orbit init` and never reused; \
         changing it would orphan every task, run, and workspace record minted under it",
    ),
    (
        "machine.task_prefix",
        "the task-id namespace is fixed for the life of this machine's task store; \
         ids already minted under it cannot be renumbered",
    ),
];

/// The refusal note for an unsettable key, or `None` for any other key.
pub(crate) fn immutable_key_note(key: &str) -> Option<&'static str> {
    IMMUTABLE_CONFIG_KEYS
        .iter()
        .find(|(immutable, _)| *immutable == key)
        .map(|(_, note)| *note)
}

/// Dotted prefix of the one table only the global `config.toml` may carry.
pub const GLOBAL_ONLY_KEY_PREFIX: &str = "machine.";

/// Whether `key` is one of the three `[machine]` identity keys, which are
/// written together by `orbit init` and never unset one at a time.
pub(crate) fn is_machine_identity_key(key: &str) -> bool {
    matches!(key, "machine.id" | "machine.name" | "machine.task_prefix")
}

/// Whether `key` names a setting a workspace `config.toml` may not supply.
pub fn is_global_only_key(key: &str) -> bool {
    key == GLOBAL_ONLY_KEY_PREFIX.trim_end_matches('.') || key.starts_with(GLOBAL_ONLY_KEY_PREFIX)
}

/// Admit a dotted key for a write through `orbit config set`.
///
/// Everything [`admit_config_key`] accepts, minus the keys that are read-only
/// after `orbit init` recorded them.
pub fn admit_settable_config_key(key: &str) -> Result<(), OrbitError> {
    admit_config_key(key)?;
    if let Some(note) = immutable_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' is read-only: {note}"
        )));
    }
    Ok(())
}

/// Admit a dotted key for `orbit config get`/`set`.
///
/// Fixed registry keys, live `crews.<name>.<field>` keys and
/// `plugins.<ns>.<key>` keys owned by an installed plugin succeed. A
/// removed key fails with its migration note; unknown registry keys and
/// misspelled crew fields fail with suggestions. All before any document
/// mutation.
pub fn admit_config_key(key: &str) -> Result<(), OrbitError> {
    if describe(key).is_some() {
        return Ok(());
    }
    if let Some(note) = removed_key_note(key) {
        return Err(OrbitError::InvalidInput(format!(
            "config key '{key}' was removed and is ignored: {note}"
        )));
    }
    if let Some(parsed) = crate::plugins::parse_plugin_field_key(key)? {
        return crate::plugins::admit_plugin_field_key(parsed);
    }
    match parse_crew_field_key(key)? {
        Some(_) => Ok(()),
        None => Err(OrbitError::invalid_input_with_suggestions(
            format!("unknown config key '{key}'"),
            all_key_names(),
        )),
    }
}

/// Parse `crews.<name>.<field>` when `key` is a crew-table path.
///
/// `None` means this is not a crew key (including the bare `crews` table).
/// An ill-formed crew path or unknown field is an error, not a fallthrough
/// to the fixed-key registry, so `crews.sol.effrot` is not reported as an
/// unknown registry setting.
pub(crate) fn parse_crew_field_key(key: &str) -> Result<Option<CrewFieldKey<'_>>, OrbitError> {
    let mut parts = key.split('.');
    if parts.next() != Some("crews") {
        return Ok(None);
    }
    let Some(name) = parts.next() else {
        return Ok(None);
    };
    let Some(field) = parts.next() else {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    };
    if parts.next().is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' has extra segments"
        )));
    }
    if name.is_empty() {
        return Err(OrbitError::InvalidInput(
            "crew config keys require a non-empty crew name".to_string(),
        ));
    }
    crate::crew_pools::reject_unpoolable_crew_name(name, "crew config keys")?;
    if field.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "crew config keys are crews.<name>.<field>; '{key}' is missing a field"
        )));
    }
    if CREW_CONFIG_FIELDS.contains(&field) {
        return Ok(Some(CrewFieldKey { name, field }));
    }
    Err(OrbitError::invalid_input_with_suggestions(
        format!("unknown crew field '{field}' in '{key}'"),
        CREW_CONFIG_FIELDS
            .iter()
            .map(|known| format!("crews.{name}.{known}"))
            .collect(),
    ))
}

/// Every settable key name, used for did-you-mean suggestions.
pub(crate) fn all_key_names() -> Vec<String> {
    CONFIG_KEY_REGISTRY
        .iter()
        .map(|entry| entry.key.to_string())
        .collect()
}
