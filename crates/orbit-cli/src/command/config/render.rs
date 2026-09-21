//! Grouped, explained rendering for `orbit config show`.
//!
//! The flat alphabetical dump this replaced buried the ~15 settings that
//! govern behaviour under 40 lines of crew fields, never printed the registry
//! description it already had for every key, rendered "no value at all" and
//! "the built-in default is in force" identically, and showed nothing about a
//! lower-layer value a workspace overrode or a global security value that was
//! deliberately not inherited.
//!
//! Three rules keep the rendering honest:
//! - a key's section comes from its registry row, so `keys` and `show` group
//!   identically and a new key cannot be added without a home;
//! - value state is three-way (`set` with its layer / `default` / `unset`),
//!   never `[unset] [built-in]`;
//! - a shadowed lower-layer value is named with the reason it lost.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CONFIG_KEY_REGISTRY, ConfigSection, ConfigSnapshot, ConfigStore, ConfigValueSourceKind,
    ConfigValueState, EffectiveConfigValue, ShadowReason, describe_config_key,
};
use orbit_core::OrbitRuntime;
use serde_json::{Map, Value as JsonValue, json};

/// Rendered in place of a value that does not exist, so an unset key is not an
/// empty column that reads as an empty string.
const NO_VALUE: &str = "–";

/// Width of the leading section/label column in the banner and section
/// headings, so the blurbs line up under each other.
const HEADING_WIDTH: usize = 28;

/// Upper bounds on the aligned columns. A longer cell is never truncated —
/// it pushes the rest of its own line right — but one very long value does
/// not indent every other row off the screen.
const MAX_LABEL_WIDTH: usize = 32;
const MAX_VALUE_WIDTH: usize = 20;

/// One rendered setting line.
struct Row {
    label: String,
    value: String,
    /// `workspace` / `global` / `environment` / `default` / `unset`, or
    /// `derived` for the non-settable invariant shown inside `Execution`.
    state: String,
    description: String,
    /// Shadowed-layer annotation, when the effective value hides one.
    note: Option<String>,
}

/// Column widths shared by every section, so the eye can scan one gutter.
struct Layout {
    label: usize,
    value: usize,
    state: usize,
}

impl Layout {
    fn measure(rows: &[Row]) -> Self {
        let width = |pick: fn(&Row) -> &str, cap: usize| {
            rows.iter()
                .map(|row| pick(row).chars().count())
                .max()
                .unwrap_or(0)
                .min(cap)
        };
        Self {
            label: width(|row| row.label.as_str(), MAX_LABEL_WIDTH),
            value: width(|row| row.value.as_str(), MAX_VALUE_WIDTH),
            state: width(|row| row.state.as_str(), usize::MAX),
        }
    }

    fn write_row(&self, out: &mut String, row: &Row) {
        let _ = write!(
            out,
            "  {:<label$}  {:<value$}  {:<state$}",
            row.label,
            row.value,
            row.state,
            label = self.label,
            value = self.value,
            state = self.state,
        );
        if !row.description.is_empty() {
            let _ = write!(out, "  {}", row.description);
        }
        if let Some(note) = &row.note {
            let _ = write!(out, "  {note}");
        }
        let _ = writeln!(out);
    }
}

/// The effective (layered) view: layers banner, workspace binding, grouped
/// sections, and resolved paths.
pub(super) fn effective_text(
    runtime: &OrbitRuntime,
    values: &[EffectiveConfigValue],
    all: bool,
) -> String {
    let mut out = String::new();
    let global_path = runtime.global_root().join("config.toml");
    let workspace_path = runtime.shared_root().join("config.toml");
    let workspace_file_exists = workspace_path.exists();

    let _ = writeln!(out, "orbit config — effective{}", workspace_suffix(runtime));
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} built-in defaults → global {} → workspace {}",
        "Layers (later wins)",
        display_path(&global_path, global_path.exists()),
        display_path(&workspace_path, workspace_file_exists),
    );
    if workspace_file_exists {
        let _ = writeln!(
            out,
            "{:<HEADING_WIDTH$} ⚠ security keys (execution.*) do not inherit from global once a \
             workspace file exists",
            ""
        );
    }
    if let Some(line) = workspace_binding_line(runtime) {
        let _ = writeln!(out);
        let _ = writeln!(out, "{line}");
    }

    // The effective view always composes the subprocess environment from the
    // allowlist; the invariant is surfaced here rather than as a settable key.
    let rows = effective_rows(values, false);
    write_sections(&mut out, &rows, values, all);
    write_paths(&mut out, runtime, None);
    out
}

/// One physical file resolved in isolation: the same grouping, without the
/// layering banner, shadowed-value notes, or crews (a scoped snapshot admits
/// registry keys only).
pub(super) fn scoped_text(
    runtime: &OrbitRuntime,
    store: &ConfigStore,
    snapshot: &ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
    all: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "orbit config — {} scope{}",
        store.scope().label(),
        workspace_suffix(runtime)
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} {}",
        "Source",
        display_path(store.path(), store.exists_on_disk())
    );
    if let Some(line) = workspace_binding_line(runtime) {
        let _ = writeln!(out, "{line}");
    }

    let rows = scoped_rows(store, settings, snapshot.execution_env_inherit);
    write_sections(&mut out, &rows, &[], all);
    write_paths(&mut out, runtime, Some(store.path()));
    out
}

/// Per-key metadata for `--json`: the existing `scope`/`path` provenance plus
/// the section, description, state, and shadowed layers the text view reads.
pub(super) fn effective_provenance_json(entry: &EffectiveConfigValue) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "scope".to_string(),
        json!(entry.source.kind().label().to_string()),
    );
    object.insert(
        "path".to_string(),
        match entry.source.path() {
            Some(path) => json!(path.to_string_lossy()),
            None => JsonValue::Null,
        },
    );
    object.insert("section".to_string(), section_token(&entry.key));
    object.insert("description".to_string(), description_json(&entry.key));
    object.insert("state".to_string(), json!(entry.state().label()));
    object.insert(
        "shadowed_by".to_string(),
        JsonValue::Array(
            entry
                .shadowed_by
                .iter()
                .map(|shadow| {
                    json!({
                        "layer": shadow.layer.label(),
                        "value": shadow.value,
                        "reason": shadow.reason.label(),
                    })
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

/// Per-key metadata for scoped `--json`. A scoped view has one layer, so the
/// state is `set` when the file defines the key and `default`/`unset`
/// otherwise, and nothing can be shadowed.
pub(super) fn scoped_provenance_json(
    store: &ConfigStore,
    key: &str,
    value: &JsonValue,
) -> JsonValue {
    json!({
        "scope": scoped_state(store, key, value).0,
        "path": if store.is_key_set(key) { json!(store.path().to_string_lossy()) } else { JsonValue::Null },
        "section": section_token(key),
        "description": description_json(key),
        "state": scoped_state(store, key, value).1,
        "shadowed_by": JsonValue::Array(Vec::new()),
    })
}

/// The registered base branch and ship mode, which delivery reads instead of
/// `workflow.base_branch`. Null for an unregistered checkout.
pub(super) fn workspace_binding_json(runtime: &OrbitRuntime) -> JsonValue {
    match runtime.workspace_runtime_binding() {
        Some(binding) => json!({
            "base_branch": binding.base_branch,
            "ship_mode": binding.ship_mode.as_input_value(),
            "repo_root": binding.repo_root.to_string_lossy(),
            "source": "workspace-registry",
        }),
        None => JsonValue::Null,
    }
}

fn write_sections(
    out: &mut String,
    rows: &[SectionRows],
    values: &[EffectiveConfigValue],
    all: bool,
) {
    let layout = Layout::measure(
        &rows
            .iter()
            .flat_map(|section| section.rows.iter())
            .map(clone_row)
            .collect::<Vec<_>>(),
    );
    for section in rows {
        let _ = writeln!(out);
        if section.section == ConfigSection::Crews {
            write_crews(out, values, section);
            continue;
        }
        let settable = section
            .rows
            .iter()
            .filter(|row| row.state != DERIVED_STATE)
            .collect::<Vec<_>>();
        let all_unset = !settable.is_empty() && settable.iter().all(|row| row.state == "unset");
        if all_unset && !all {
            let _ = writeln!(
                out,
                "{:<HEADING_WIDTH$} {} — {} keys unset (pass --all to list them)",
                section.section.title(),
                section.section.blurb(),
                settable.len()
            );
            continue;
        }
        let _ = writeln!(
            out,
            "{:<HEADING_WIDTH$} {}{}",
            section.section.title(),
            section.section.blurb(),
            section.heading_suffix
        );
        for row in &section.rows {
            layout.write_row(out, row);
        }
    }
}

/// `state` of the derived `execution.env.inherit` invariant: it is not a
/// settable key, so it is neither set, defaulted, nor unset.
const DERIVED_STATE: &str = "derived";

struct SectionRows {
    section: ConfigSection,
    rows: Vec<Row>,
    /// Appended to the section heading, for the execution security warning.
    heading_suffix: String,
}

fn clone_row(row: &Row) -> Row {
    Row {
        label: row.label.clone(),
        value: row.value.clone(),
        state: row.state.clone(),
        description: row.description.clone(),
        note: row.note.clone(),
    }
}

fn effective_rows(values: &[EffectiveConfigValue], env_inherit: bool) -> Vec<SectionRows> {
    let mut sections = Vec::new();
    let workspace_present = values
        .iter()
        .any(|entry| entry.source.kind() == ConfigValueSourceKind::Workspace);
    for section in ConfigSection::ORDER {
        let mut rows = Vec::new();
        if *section != ConfigSection::Crews {
            for descriptor in ordered_keys(*section) {
                let Some(entry) = values.iter().find(|entry| entry.key == descriptor.key) else {
                    continue;
                };
                rows.push(Row {
                    label: label_for(*section, &entry.key),
                    value: render_value(&entry.value),
                    state: effective_state(entry),
                    description: descriptor.description.to_string(),
                    note: shadow_note(entry),
                });
            }
        }
        if *section == ConfigSection::Execution {
            rows.push(env_inherit_row(env_inherit));
        }
        let has_crews = *section == ConfigSection::Crews
            && values.iter().any(|entry| entry.key.starts_with("crews."));
        if rows.is_empty() && !has_crews && *section != ConfigSection::Crews {
            continue;
        }
        sections.push(SectionRows {
            section: *section,
            rows,
            heading_suffix: if *section == ConfigSection::Execution && workspace_present {
                "          ⚠ workspace file present: global execution values are ignored"
                    .to_string()
            } else {
                String::new()
            },
        });
    }
    sections
}

fn scoped_rows(
    store: &ConfigStore,
    settings: &[(&'static str, JsonValue)],
    env_inherit: bool,
) -> Vec<SectionRows> {
    let mut sections = Vec::new();
    for section in ConfigSection::ORDER {
        if *section == ConfigSection::Crews {
            continue;
        }
        let mut rows = Vec::new();
        for descriptor in ordered_keys(*section) {
            let Some((key, value)) = settings.iter().find(|(key, _)| *key == descriptor.key) else {
                continue;
            };
            rows.push(Row {
                label: label_for(*section, key),
                value: render_value(value),
                state: scoped_state(store, key, value).1.to_string(),
                description: descriptor.description.to_string(),
                note: None,
            });
        }
        if *section == ConfigSection::Execution {
            rows.push(env_inherit_row(env_inherit));
        }
        if rows.is_empty() {
            continue;
        }
        sections.push(SectionRows {
            section: *section,
            rows,
            heading_suffix: String::new(),
        });
    }
    sections
}

/// `execution.env.inherit` is a derived invariant rather than a settable key
/// (see `orbit_config::ExecutionEnvPolicy`), so it is rendered inside the
/// section it governs but labelled as not settable.
fn env_inherit_row(inherit: bool) -> Row {
    Row {
        label: "env.inherit".to_string(),
        value: inherit.to_string(),
        state: DERIVED_STATE.to_string(),
        description: "Not settable: an agent subprocess environment is always composed from \
                      execution.env.pass."
            .to_string(),
        note: None,
    }
}

fn write_crews(out: &mut String, values: &[EffectiveConfigValue], section: &SectionRows) {
    let crews = crew_table(values);
    if crews.is_empty() {
        let _ = writeln!(
            out,
            "{:<HEADING_WIDTH$} {} — none defined",
            section.section.title(),
            section.section.blurb()
        );
        return;
    }
    let layers = crews
        .iter()
        .map(|crew| crew.layer.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let origin = if layers.len() == 1 {
        layers
            .iter()
            .next()
            .map(|layer| format!("all from {layer}"))
            .unwrap_or_default()
    } else {
        format!(
            "from {}",
            layers.into_iter().collect::<Vec<_>>().join(" and ")
        )
    };
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} {} defined, {origin}",
        section.section.title(),
        crews.len()
    );

    let columns: [&str; 6] = ["NAME", "FROM", "PROVIDER", "MODEL", "EFFORT", "TAGS"];
    let mut widths = columns.map(|column| column.chars().count());
    for crew in &crews {
        for (index, cell) in crew.cells().iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let write_cells = |out: &mut String, cells: &[String; 6], trailing: &str| {
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            let _ = write!(line, "  {:<width$}", cell, width = widths[index]);
        }
        let line = line.trim_end().to_string();
        let _ = writeln!(out, "{line}{trailing}");
    };
    write_cells(out, &columns.map(str::to_string), "  DESCRIPTION");
    for crew in &crews {
        let mut trailing = String::new();
        if !crew.description.is_empty() {
            let _ = write!(trailing, "  {}", crew.description);
        }
        if !crew.annotation.is_empty() {
            let _ = write!(trailing, "  ← {}", crew.annotation);
        }
        write_cells(out, &crew.cells(), &trailing);
    }
}

struct CrewRow {
    name: String,
    layer: String,
    provider: String,
    model: String,
    effort: String,
    tags: String,
    description: String,
    /// Which `workflow.*` keys point at this crew.
    annotation: String,
}

impl CrewRow {
    fn cells(&self) -> [String; 6] {
        [
            self.name.clone(),
            self.layer.clone(),
            self.provider.clone(),
            self.model.clone(),
            self.effort.clone(),
            self.tags.clone(),
        ]
    }
}

/// One row per crew, folded from the per-field `crews.<name>.<field>` values.
fn crew_table(values: &[EffectiveConfigValue]) -> Vec<CrewRow> {
    let referenced = |key: &str| {
        values
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.as_str().map(str::to_string))
    };
    let default_crew = referenced("workflow.default_crew");
    let system_crew = referenced("workflow.system_crew");

    let mut fields: BTreeMap<String, BTreeMap<String, (String, ConfigValueSourceKind)>> =
        BTreeMap::new();
    for entry in values {
        let Some(rest) = entry.key.strip_prefix("crews.") else {
            continue;
        };
        let Some((name, field)) = rest.split_once('.') else {
            continue;
        };
        fields.entry(name.to_string()).or_default().insert(
            field.to_string(),
            (render_value(&entry.value), entry.source.kind()),
        );
    }

    fields
        .into_iter()
        .map(|(name, crew)| {
            let cell = |field: &str| {
                crew.get(field)
                    .map(|(value, _)| value.clone())
                    // An empty list is nothing configured, not a value of "[]".
                    .filter(|value| value != "[]")
                    .unwrap_or_else(|| NO_VALUE.to_string())
            };
            // Every crew has built-in projections for the fields it omits, so
            // only the layers that actually define a field are provenance.
            let mut layers = crew
                .values()
                .map(|(_, kind)| kind.label())
                .filter(|label| *label != ConfigValueSourceKind::BuiltIn.label())
                .collect::<Vec<_>>();
            layers.sort_unstable();
            layers.dedup();
            if layers.is_empty() {
                layers.push(ConfigValueSourceKind::BuiltIn.label());
            }
            let mut annotation = Vec::new();
            if default_crew.as_deref() == Some(name.as_str()) {
                annotation.push("workflow.default_crew");
            }
            if system_crew.as_deref() == Some(name.as_str()) {
                annotation.push("workflow.system_crew");
            }
            CrewRow {
                layer: layers.join("+"),
                provider: cell("provider"),
                model: cell("model"),
                effort: cell("effort"),
                tags: cell("tags"),
                description: crew
                    .get("description")
                    .map(|(value, _)| value.clone())
                    .filter(|value| value != NO_VALUE)
                    .unwrap_or_default(),
                annotation: annotation.join(", "),
                name,
            }
        })
        .collect()
}

/// The resolved roots and store locations, replacing the old `derived:` block
/// and its single-line `persistence` JSON blob. The rows themselves come from
/// `orbit_core::application::config`, so this view and the dashboard's Paths
/// grid name the same locations.
fn write_paths(out: &mut String, runtime: &OrbitRuntime, config_path: Option<&Path>) {
    let rows = orbit_core::application::config::path_rows(runtime, config_path);
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let _ = writeln!(out);
    let _ = writeln!(out, "Paths");
    for (label, value) in rows {
        let _ = writeln!(out, "  {label:<width$}  {value}");
    }
}

/// Registry rows for one section, most relevant first.
fn ordered_keys(section: ConfigSection) -> Vec<&'static orbit_config::ConfigKeyDescriptor> {
    let mut keys = CONFIG_KEY_REGISTRY
        .iter()
        .filter(|descriptor| descriptor.section == section)
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| left.order.cmp(&right.order).then(left.key.cmp(right.key)));
    keys
}

fn label_for(section: ConfigSection, key: &str) -> String {
    match section.key_prefix() {
        Some(prefix) => key
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(key)
            .to_string(),
        None => key.to_string(),
    }
}

fn section_token(key: &str) -> JsonValue {
    if key.starts_with("crews.") {
        return json!(ConfigSection::Crews.token());
    }
    match describe_config_key(key) {
        Some(descriptor) => json!(descriptor.section.token()),
        None => JsonValue::Null,
    }
}

fn description_json(key: &str) -> JsonValue {
    match describe_config_key(key) {
        Some(descriptor) => json!(descriptor.description),
        None => JsonValue::Null,
    }
}

/// The layer that set the value, or the three-way state when no layer did.
fn effective_state(entry: &EffectiveConfigValue) -> String {
    match entry.state() {
        ConfigValueState::Set => entry.source.kind().label().to_string(),
        state => state.label().to_string(),
    }
}

/// `(scope, state)` for a scoped view: the file's own scope when it defines
/// the key, else the same three-way state as the effective view.
fn scoped_state(store: &ConfigStore, key: &str, value: &JsonValue) -> (&'static str, &'static str) {
    if store.is_key_set(key) {
        return (store.scope().label(), ConfigValueState::Set.label());
    }
    if value.is_null() {
        return ("built-in", ConfigValueState::Unset.label());
    }
    ("built-in", ConfigValueState::Default.label())
}

fn shadow_note(entry: &EffectiveConfigValue) -> Option<String> {
    let shadow = entry.shadowed_by.first()?;
    let layer = shadow.layer.label();
    let value = render_value(&shadow.value);
    Some(match shadow.reason {
        ShadowReason::Overridden => format!("(overrides {layer}: {value})"),
        ShadowReason::NotInherited => format!("({layer} sets {value} — not inherited)"),
        ShadowReason::PresetReset => {
            format!("({layer} sets {value} — reset by workspace operation.preset)")
        }
    })
}

fn render_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => NO_VALUE.to_string(),
        JsonValue::String(text) if text.is_empty() => "\"\"".to_string(),
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) if items.is_empty() => "[]".to_string(),
        JsonValue::Array(items) => items
            .iter()
            .map(render_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

fn path_cell(path: &Path) -> String {
    redact_home_dir(&path.display().to_string())
}

fn display_path(path: &Path, exists: bool) -> String {
    let rendered = path_cell(path);
    if exists {
        rendered
    } else {
        format!("{rendered} (absent)")
    }
}

/// `(workspace: <name>)` for a registered checkout, taken from the repo root
/// directory name rather than an internal workspace id.
fn workspace_suffix(runtime: &OrbitRuntime) -> String {
    runtime
        .workspace_runtime_binding()
        .and_then(|binding| binding.repo_root.file_name().map(|name| name.to_owned()))
        .map(|name| format!(" (workspace: {})", name.to_string_lossy()))
        .unwrap_or_default()
}

/// The registry's own delivery values. They are what delivery uses, so a
/// mismatch against `workflow.base_branch` has to be visible here.
fn workspace_binding_line(runtime: &OrbitRuntime) -> Option<String> {
    let binding = runtime.workspace_runtime_binding()?;
    let base_branch = binding
        .base_branch
        .clone()
        .unwrap_or_else(|| "–".to_string());
    Some(format!(
        "{:<HEADING_WIDTH$} registered base branch: {base_branch}   ship mode: {}   (from \
         workspace registry, not config.toml)",
        "Workspace",
        binding.ship_mode.as_input_value(),
    ))
}
