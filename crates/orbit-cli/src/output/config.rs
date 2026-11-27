//! Human and structured output for `orbit config` (`show`, `get`, `set`,
//! `keys`, and `path`), including terminal-width fitting for `orbit config show`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CONFIG_KEY_REGISTRY, ConfigKeyDescriptor, ConfigKeyTier, ConfigSection, ConfigSnapshot,
    ConfigStore, ConfigValueSourceKind, ConfigValueState, EffectiveConfigValue, ShadowReason,
    describe_config_key,
};
use orbit_core::app::config::{ConfigGetResult, ConfigShowData, ConfigShowRoots};
use serde_json::{Map, Value as JsonValue, json};

use crate::output::common::payload::{Block, Payload};
use crate::output::common::sink::OutputMode;
use crate::output::common::table::{Column, Table};

/// Minimum columns a description column must leave before aligned wrapping
/// gives way to a shallower hanging indent.
const CONFIG_SHOW_MIN_TRAILING: usize = 24;

/// Rendered in place of a value that does not exist, so an unset key is not an
/// empty column that reads as an empty string.
const NO_VALUE: &str = "–";

/// Width of the leading section/label column in the banner and section
/// headings, so the blurbs line up under each other.
const HEADING_WIDTH: usize = 28;

/// Upper bounds on the aligned columns. A longer cell is never truncated in
/// the text this module builds — it pushes the rest of its own line right —
/// but one very long value does not indent every other row off the screen.
/// The output block fits this text to the terminal width on a terminal; piped
/// and plain output keeps every line whole.
const MAX_LABEL_WIDTH: usize = 32;

const MAX_VALUE_WIDTH: usize = 20;

/// `state` of the derived `execution.env.inherit` invariant: it is not a
/// settable key, so it is neither set, defaulted, nor unset.
const DERIVED_STATE: &str = "derived";

/// Text-view state for `machine.id` and `machine.task_prefix`. They are
/// identity facts `orbit init` writes once, not settings in the Machine table.
const IDENTITY_STATE: &str = "identity";

/// Aligned configuration text with trailing-prose boundaries supplied by its producer.
/// Offsets refer to UTF-8 line starts; columns count characters, as the formatter does.
#[derive(Debug, Default)]
pub struct ConfigShow {
    pub(crate) text: String,
    pub(crate) trailing_columns: BTreeMap<usize, usize>,
}

impl ConfigShow {
    /// Append verbatim text without changing previously recorded boundaries.
    pub(crate) fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Mark the current position as this line's trailing prose column.
    /// Call after writing all padded cells and their final gutter.
    pub(crate) fn mark_trailing_column(&mut self) {
        let line_start = self.text.rfind('\n').map_or(0, |index| index + 1);
        let column = self.text[line_start..].chars().count();
        self.trailing_columns.insert(line_start, column);
    }
}

impl std::fmt::Write for ConfigShow {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.push_str(text);
        Ok(())
    }
}

/// Every automatic-review switch with its source [ORB-13992] [ORB-20099]:
/// before-PR review (deprecated `review.timing = before_pr`, minutes, crew),
/// before-landing review (`review.timing = before_landing`) and after-landing
/// review (the `delivery-code-review` auto-task, its next batch and health).
/// The before-PR line marks its deprecated timing; deprecation alone is never
/// an error.
struct ReviewView {
    json: JsonValue,
    text: String,
}

/// The effective `[workflow.review_skip_tags]` mappings: each tag's
/// `paths` and optional `guard`. Empty text when none is mapped (the
/// JSON still carries an empty object).
struct ReviewSkipView {
    json: JsonValue,
    text: String,
}

/// One rendered setting line.
struct Row {
    label: String,
    value: String,
    /// `workspace` / `global` / `environment` / `default` / `unset`,
    /// `derived` for the non-settable invariant inside Execution, or
    /// `identity` for `machine.id` and `machine.task_prefix`.
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

    fn write_row(&self, out: &mut ConfigShow, row: &Row) {
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
            out.push_str("  ");
            out.mark_trailing_column();
            out.push_str(&row.description);
        }
        if let Some(note) = &row.note {
            out.push_str("  ");
            if row.description.is_empty() {
                out.mark_trailing_column();
            }
            out.push_str(note);
        }
        let _ = writeln!(out);
    }
}

struct SectionRows {
    section: ConfigSection,
    rows: Vec<Row>,
    /// Appended to the section heading, for the execution security warning.
    heading_suffix: String,
    hidden_advanced: usize,
}

struct CrewRow {
    name: String,
    /// `true` / `false`: every crew row carries its effective enabled state.
    enabled: String,
    layer: String,
    provider: String,
    model: String,
    effort: String,
    tags: String,
    description: String,
    /// `env` entries (`NAME` / `NAME=SOURCE`, names only), empty when none.
    env: String,
    /// Which `workflow.*` keys point at this crew.
    annotation: String,
}

impl CrewRow {
    fn cells(&self) -> [String; 7] {
        [
            self.name.clone(),
            self.enabled.clone(),
            self.layer.clone(),
            self.provider.clone(),
            self.model.clone(),
            self.effort.clone(),
            self.tags.clone(),
        ]
    }
}

/// Build the payload for `orbit config show`.
pub(crate) fn show_payload(shown: ConfigShowData, all: bool) -> Payload {
    match shown {
        ConfigShowData::Effective {
            values,
            review,
            crew_purposes,
            review_skip_tags,
            roots,
        } => {
            let review = review_switches(review);
            let mut json = effective_json(&roots, &values);
            json["review"] = review.json;
            let review_skip = review_skip_view(&review_skip_tags);
            json["review_skip_tags"] = review_skip.json;
            let mut text = effective_text(&roots, &values, all);
            if all || !review_skip.text.is_empty() {
                text.push_str(&review_skip.text);
            }
            // Crew purposes and Review restate rows the sections above already
            // print. The default view omits them; `--all` and `--json` keep them.
            if all {
                text.push_str(&crew_purposes_text(&crew_purposes));
                text.push_str(&review.text);
            }
            json["crew_purposes"] = crew_purposes;
            Payload::blocks(json, vec![config_show_block(text)])
        }
        ConfigShowData::Scoped {
            store,
            snapshot,
            roots,
        } => {
            let settings = snapshot.all_values();
            Payload::blocks(
                scoped_json(&roots, &store, &snapshot, &settings),
                vec![config_show_block(scoped_text(
                    &roots, &store, &snapshot, &settings, all,
                ))],
            )
        }
    }
}

/// Build the payload for `orbit config get`.
pub(crate) fn get_payload(key: &str, result: ConfigGetResult) -> Payload {
    match result {
        ConfigGetResult::Effective { value } => {
            let text = format_value_for_display(&value);
            Payload::detail(
                json!({
                    "key": key,
                    "scope": "effective",
                    "value": value,
                }),
                text,
            )
        }
        ConfigGetResult::Scoped {
            scope,
            path,
            value,
            exists,
        } => {
            let path = match path {
                Some(path) => JsonValue::String(path.to_string_lossy().into_owned()),
                None => JsonValue::Null,
            };
            let text = format_value_for_display(&value);
            Payload::detail(
                json!({
                    "key": key,
                    "scope": scope,
                    "path": path,
                    "value": value,
                    "exists": exists,
                }),
                text,
            )
        }
    }
}

/// Build the payload for `orbit config set`.
pub(crate) fn set_payload(key: &str, scope: &str, path: &Path, warnings: &[String]) -> Payload {
    let warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    let path = path.to_string_lossy().into_owned();
    let mut document = json!({ "key": key, "scope": scope, "path": path });
    if let Some(warning) = &warning {
        document["warning"] = json!(warning);
    }
    Payload::detail(
        document,
        format!(
            "set {key} ({scope} config: {path}){}",
            warning
                .map(|warning| format!("\nWarning: {warning}"))
                .unwrap_or_default()
        ),
    )
}

/// Build the payload for `orbit config keys`.
pub(crate) fn keys_payload() -> Payload {
    let keys: Vec<_> = CONFIG_KEY_REGISTRY
        .iter()
        .map(|entry| {
            json!({
                "key": entry.key,
                "type": entry.value_type,
                "section": entry.section.token(),
                "description": entry.description,
                "detail": entry.detail,
                "allowed": entry.allowed.to_json(),
            })
        })
        .collect();
    let mut table = Table::new(vec![
        Column::new("KEY").fixed(),
        Column::new("TYPE").fixed(),
        Column::new("SECTION").fixed(),
        Column::new("DESCRIPTION"),
        Column::new("ALLOWED"),
    ])
    .empty_message("no config keys registered");

    for entry in CONFIG_KEY_REGISTRY.iter() {
        let allowed = entry.allowed.summary().unwrap_or_default();
        table.add_row(vec![
            entry.key,
            entry.value_type,
            entry.section.token(),
            entry.description,
            allowed.as_str(),
        ]);
    }

    Payload::enveloped_list(json!({ "keys": keys.clone() }), keys, table)
}

/// Build the payload for `orbit config path`.
pub(crate) fn path_payload(path: &Path) -> Payload {
    let path = path.to_string_lossy().into_owned();
    Payload::detail(json!({ "path": path }), path.clone())
}

/// Build a `config show` block that fits trailing prose columns to the terminal
/// width in table mode while keeping plain mode unwrapped.
pub(crate) fn config_show_block(mut show: ConfigShow) -> Block {
    // One trailing newline is dropped because the renderer adds its own when
    // it prints the block.
    if show.text.ends_with('\n') {
        show.text.pop();
    }
    Block::custom(move |sink| {
        if sink.mode() == OutputMode::Table {
            println!("{}", config_show_text(&show, sink.width()));
        } else {
            println!("{}", show.text);
        }
    })
}

/// Fit `config show` text to `width` columns. Lines that fit are left exactly
/// as written, so aligned columns survive. A longer line wraps its trailing
/// prose at word boundaries with continuations aligned to that prose's
/// column: the blurb column of a heading (after the padded title), the
/// description column of a key row (after label/value/state), the trailing
/// column of a crew row, or the value column of a Paths row. When that column
/// would leave too little room — narrow terminals where the aligned columns
/// themselves approach the width — the whole line wraps with a shallow
/// hanging indent instead, so no line exceeds the width. Width `0` (no
/// terminal width) leaves the text whole.
fn config_show_text(text: &ConfigShow, width: u16) -> String {
    if width == 0 {
        return text.text.clone();
    }
    let mut line_start = 0;
    text.text
        .split('\n')
        .map(|line| {
            let column = text.trailing_columns.get(&line_start).copied();
            line_start += line.len() + 1;
            fit_config_show_line(line, usize::from(width), column)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fit_config_show_line(line: &str, width: usize, trailing_column: Option<usize>) -> String {
    if line.chars().count() <= width {
        return line.to_string();
    }
    // Row boundaries come from the command's padded cells, never word positions:
    // values, tags and path labels may all contain whitespace.
    let column = trailing_column.or_else(|| {
        // Headings retain their fixed padded title/blurb layout.
        if line.starts_with(' ') {
            None
        } else {
            find_blurb_start(line)
        }
    });
    let aligned = column.map(|column| {
        let split = byte_at(line, column);
        (line[..split].to_string(), line[split..].to_string(), column)
    });
    if let Some((prefix, trailing, column)) = aligned
        && prefix.chars().count() + 8 <= width
    {
        // Keep the row's columns on the first line. Continuations align to
        // the trailing column when it leaves room; on a narrow terminal
        // they fall back to a shallow indent so the prose gets full width.
        let indent = if column + CONFIG_SHOW_MIN_TRAILING <= width {
            column
        } else {
            4.min(width.saturating_sub(1))
        };
        return wrap_trailing(&prefix, &trailing, indent, width);
    }
    // Fallback: wrap the whole line with a hanging indent two columns deeper
    // than its own indent (column 0 for headings).
    let lead = line.chars().take_while(|ch| *ch == ' ').count();
    let indent = if lead > 0 { lead + 2 } else { 0 };
    let indent = if indent + 1 >= width {
        lead.min(width.saturating_sub(1))
    } else {
        indent
    };
    let lead = lead.min(width.saturating_sub(1));
    wrap_trailing(
        &" ".repeat(lead),
        line.trim_start_matches(' '),
        indent,
        width,
    )
}

/// Byte index of the `char_offset`-th char of `line` (its length when beyond).
fn byte_at(line: &str, char_offset: usize) -> usize {
    line.char_indices()
        .nth(char_offset)
        .map_or(line.len(), |(byte, _)| byte)
}

/// Char offset where a heading's blurb starts: the end of the first run of
/// two or more spaces, when any text follows it.
fn find_blurb_start(line: &str) -> Option<usize> {
    let chars: Vec<char> = line.chars().collect();
    let mut run = 0;
    for (index, ch) in chars.iter().enumerate() {
        if *ch == ' ' {
            run += 1;
            if run >= 2 {
                // End of this run.
                let mut end = index + 1;
                while end < chars.len() && chars[end] == ' ' {
                    end += 1;
                }
                if end < chars.len() {
                    return Some(end);
                }
                return None;
            }
        } else {
            run = 0;
        }
    }
    None
}

/// Wrap `trailing` at word boundaries into lines of at most `width` chars:
/// the first follows `prefix`, continuations sit at `indent`. A word moves to
/// the next line whole when it fits there, even if it is too wide for the
/// first line's remainder; only a word wider than a continuation line is split.
fn wrap_trailing(prefix: &str, trailing: &str, indent: usize, width: usize) -> String {
    let available_first = width.saturating_sub(prefix.chars().count()).max(1);
    let available_rest = width.saturating_sub(indent).max(1);
    let mut wrapped: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut available = available_first;
    let push_word =
        |current: &mut String, wrapped: &mut Vec<String>, available: &mut usize, word: &str| {
            let mut rest = word;
            loop {
                let room = available.saturating_sub(current.chars().count());
                if current.is_empty() {
                    if rest.chars().count() <= *available {
                        current.push_str(rest);
                        return;
                    }
                    // A first word too wide for the first line's remainder moves
                    // whole to a continuation when it fits there; the first line
                    // then keeps only its prefix (see the assembly below).
                    if wrapped.is_empty() && rest.chars().count() <= available_rest {
                        wrapped.push(String::new());
                        *available = available_rest;
                        continue;
                    }
                    // Split a word wider than a whole line.
                    let take = *available;
                    let split = byte_at(rest, take);
                    current.push_str(&rest[..split]);
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                    rest = &rest[split..];
                } else if rest.chars().count() < room {
                    current.push(' ');
                    current.push_str(rest);
                    return;
                } else {
                    wrapped.push(std::mem::take(current));
                    *available = available_rest;
                }
            }
        };
    for word in trailing.split_whitespace() {
        push_word(&mut current, &mut wrapped, &mut available, word);
    }
    wrapped.push(current);
    let mut out = String::new();
    for (index, chunk) in wrapped.iter().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
        } else if chunk.is_empty() && wrapped.len() > 1 {
            // The first word moved to a continuation: no trailing gutter is left behind.
            out.push_str(prefix.trim_end());
        } else {
            out.push_str(prefix);
        }
        out.push_str(chunk);
    }
    out
}

fn review_switches(json: JsonValue) -> ReviewView {
    let text = match json["error"].as_str() {
        Some(error) => format!("\nReview: {error}\n"),
        None => format!(
            "\nReview:\n  before-PR: {}\n  before-landing: {}\n  after-landing: {}\n",
            json["before_pr"]["line"].as_str().unwrap_or_default(),
            json["before_landing"]["line"].as_str().unwrap_or_default(),
            json["after_landing"]["line"].as_str().unwrap_or_default(),
        ),
    };
    ReviewView { json, text }
}

fn review_skip_view(entries: &[(String, Vec<String>, Option<String>)]) -> ReviewSkipView {
    let mut map = Map::new();
    let mut text = String::new();
    if !entries.is_empty() {
        text.push_str("\nReview skip tags:\n");
        for (tag, paths, guard) in entries {
            map.insert(tag.clone(), json!({"paths": paths, "guard": guard}));
            let guard = guard.as_deref().unwrap_or("none");
            text.push_str(&format!(
                "  {tag}: paths=[{}] guard={guard}\n",
                paths.join(", ")
            ));
        }
    }
    ReviewSkipView {
        json: JsonValue::Object(map),
        text,
    }
}

/// The `Crew purposes:` block `orbit config show` prints.
fn crew_purposes_text(view: &JsonValue) -> String {
    let mut text = String::from("\nCrew purposes:\n");
    for row in view.as_array().into_iter().flatten() {
        let field = |name: &str| row[name].as_str().unwrap_or("-");
        let pool = row["pool"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(JsonValue::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let line = if row["configured"].as_bool() == Some(true) {
            format!("{pool} ({}, {})", field("key"), field("source"))
        } else if field("resolved_from") == field("key") {
            format!("none ({} is empty or unset)", field("key"))
        } else if row["source"].is_null() {
            format!("unset; falls back to {}", field("resolved_from"))
        } else if pool.is_empty() {
            format!(
                "unset; falls back to {} (empty, so workflow.default_crew)",
                field("resolved_from")
            )
        } else {
            format!(
                "unset; falls back to {} [{pool}] ({})",
                field("resolved_from"),
                field("source")
            )
        };
        text.push_str(&format!("  {}: {line}\n", field("purpose")));
    }
    text
}

fn effective_json(roots: &ConfigShowRoots, values: &[EffectiveConfigValue]) -> JsonValue {
    let mut settings = Map::new();
    let mut provenance = Map::new();
    for entry in values {
        settings.insert(entry.key.clone(), entry.value.clone());
        // `provenance` is the per-key record: the pre-existing `scope`/`path`
        // fields keep their meaning and position, and the grouping, registry
        // description, three-way state, and shadowed layers are added
        // alongside them rather than in a parallel map.
        provenance.insert(entry.key.clone(), effective_provenance_json(entry));
    }
    let global_path = roots.global_root.join("config.toml");
    let workspace_path = roots.shared_root.join("config.toml");
    let global_exists = global_path.exists();
    let workspace_exists = workspace_path.exists() && roots.has_workspace_layer;
    let config_path = if workspace_exists {
        &workspace_path
    } else {
        &global_path
    };

    json!({
        "source": {
            "scope": "effective",
            "global_path": global_path.to_string_lossy(),
            "global_exists": global_exists,
            "workspace_path": workspace_path.to_string_lossy(),
            "workspace_exists": workspace_exists,
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings,
        "provenance": provenance,
        "workspace_binding": workspace_binding_json(roots),
        "execution_env_inherit": false,
        "global_root": roots.global_root.to_string_lossy(),
        "shared_root": roots.shared_root.to_string_lossy(),
        "local_root": roots.local_root.to_string_lossy(),
        "config_path": config_path.to_string_lossy(),
        "persistence": roots.persistence,
    })
}

fn scoped_json(
    roots: &ConfigShowRoots,
    store: &ConfigStore,
    snapshot: &ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
) -> JsonValue {
    let mut settings_obj = Map::new();
    let mut provenance = Map::new();
    for (key, value) in settings {
        settings_obj.insert((*key).to_string(), value.clone());
        provenance.insert(
            (*key).to_string(),
            scoped_provenance_json(store, key, value),
        );
    }

    // `global_root`/`shared_root`/`local_root`/`config_path`/`persistence`
    // stay top-level (not nested under a `derived` object) because an
    // existing contract test (`worktree_resolution.rs`) already asserts on
    // `shared_root`/`local_root` at the top level of `config show --json`,
    // and explicitly forbids reintroducing renamed aliases for them. The
    // grouped `Paths` section the task asks for is rendered in the human-
    // readable text output below; the JSON shape keeps these pre-existing
    // field names and positions unchanged.
    json!({
        "source": {
            "scope": store.scope().label(),
            "path": store.path().to_string_lossy(),
            "exists": store.path().exists(),
        },
        "shadowed_global_path": JsonValue::Null,
        "settings": settings_obj,
        "provenance": provenance,
        "workspace_binding": workspace_binding_json(roots),
        "execution_env_inherit": snapshot.execution_env_inherit,
        "global_root": roots.global_root.to_string_lossy(),
        "shared_root": roots.shared_root.to_string_lossy(),
        "local_root": roots.local_root.to_string_lossy(),
        "config_path": store.path().to_string_lossy(),
        "persistence": roots.persistence,
    })
}

/// The effective (layered) view: layers banner, workspace binding and grouped
/// sections. `--all` also appends resolved paths.
fn effective_text(
    roots: &ConfigShowRoots,
    values: &[EffectiveConfigValue],
    all: bool,
) -> ConfigShow {
    let mut out = ConfigShow::default();
    let global_path = roots.global_root.join("config.toml");
    let workspace_path = roots.shared_root.join("config.toml");
    let has_workspace_layer = roots.has_workspace_layer;
    let workspace_file_exists = has_workspace_layer && workspace_path.exists();

    let _ = writeln!(out, "orbit config — effective{}", workspace_suffix(roots));
    let _ = writeln!(out);
    let _ = write!(
        out,
        "{:<HEADING_WIDTH$} built-in defaults → global {}",
        "Layers (later wins)",
        display_path(&global_path, global_path.exists())
    );
    if has_workspace_layer {
        let _ = write!(
            out,
            " → workspace {}",
            display_path(&workspace_path, workspace_file_exists)
        );
    }
    let _ = writeln!(out);
    // A workspace file holding only `[plugin_enablement]` is not a policy
    // layer, so the exception is not in force for it.
    if workspace_file_exists && orbit_config::workspace_config_sets_policy(&workspace_path) {
        let _ = writeln!(
            out,
            "{:<HEADING_WIDTH$} ⚠ security keys (execution.*) do not inherit from global once a \
             workspace file exists",
            ""
        );
    }
    if let Some(line) = workspace_binding_line(roots) {
        let _ = writeln!(out);
        let _ = writeln!(out, "{line}");
    }

    // The effective view always composes the subprocess environment from the
    // allowlist; the invariant is surfaced here rather than as a settable key.
    let (identity, rows) = effective_rows(values, false, all);
    write_sections(&mut out, &rows, values, all, &identity);
    // Paths restates the roots already named in the layers banner. `--all`
    // keeps the block; the default view does not.
    if all {
        write_paths(&mut out, &roots.path_rows);
    }
    out
}

/// One physical file resolved in isolation: the same grouping, without the
/// layering banner, shadowed-value notes, or crews (a scoped snapshot admits
/// registry keys only).
fn scoped_text(
    roots: &ConfigShowRoots,
    store: &ConfigStore,
    snapshot: &ConfigSnapshot,
    settings: &[(&'static str, JsonValue)],
    all: bool,
) -> ConfigShow {
    let mut out = ConfigShow::default();
    let _ = writeln!(
        out,
        "orbit config — {} scope{}",
        store.scope().label(),
        workspace_suffix(roots)
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} {}",
        "Source",
        display_path(store.path(), store.exists_on_disk())
    );
    if let Some(line) = workspace_binding_line(roots) {
        let _ = writeln!(out, "{line}");
    }

    let (identity, rows) = scoped_rows(store, settings, snapshot.execution_env_inherit, all);
    write_sections(&mut out, &rows, &[], all, &identity);
    if all {
        write_paths(&mut out, &roots.path_rows);
    }
    out
}

/// Per-key metadata for `--json`: the existing `scope`/`path` provenance plus
/// the section, description, state, and shadowed layers the text view reads.
fn effective_provenance_json(entry: &EffectiveConfigValue) -> JsonValue {
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
    object.insert("tier".to_string(), tier_json(&entry.key));
    object.insert("description".to_string(), description_json(&entry.key));
    object.insert("allowed".to_string(), allowed_json(&entry.key));
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
fn scoped_provenance_json(store: &ConfigStore, key: &str, value: &JsonValue) -> JsonValue {
    let path = if store.is_key_set(key) {
        json!(store.path().to_string_lossy())
    } else {
        JsonValue::Null
    };
    json!({
        "scope": scoped_state(store, key, value).0,
        "path": path,
        "section": section_token(key),
        "tier": tier_json(key),
        "description": description_json(key),
        "allowed": allowed_json(key),
        "state": scoped_state(store, key, value).1,
        "shadowed_by": JsonValue::Array(Vec::new()),
    })
}

/// The registered base branch and ship mode, which delivery reads instead of
/// `workflow.base_branch`. Null for an unregistered checkout.
fn workspace_binding_json(roots: &ConfigShowRoots) -> JsonValue {
    match &roots.workspace_binding {
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
    out: &mut ConfigShow,
    rows: &[SectionRows],
    values: &[EffectiveConfigValue],
    all: bool,
    identity: &[Row],
) {
    let mut measured = identity.iter().map(clone_row).collect::<Vec<_>>();
    measured.extend(
        rows.iter()
            .flat_map(|section| section.rows.iter())
            .map(clone_row),
    );
    let layout = Layout::measure(&measured);
    write_identity(out, &layout, identity);
    for section in rows {
        let hidden = if section.hidden_advanced == 0 {
            String::new()
        } else {
            format!(
                " — {} advanced hidden (pass --all or --advanced to list them)",
                section.hidden_advanced
            )
        };
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
                "{:<HEADING_WIDTH$} {} — {} keys unset (pass --all to list them){hidden}",
                section.section.title(),
                section.section.blurb(),
                settable.len()
            );
            continue;
        }
        let _ = writeln!(
            out,
            "{:<HEADING_WIDTH$} {}{}{hidden}",
            section.section.title(),
            section.section.blurb(),
            section.heading_suffix
        );
        for row in &section.rows {
            layout.write_row(out, row);
        }
    }
}

fn is_show_identity_key(key: &str) -> bool {
    matches!(key, "machine.id" | "machine.task_prefix")
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

fn effective_rows(
    values: &[EffectiveConfigValue],
    env_inherit: bool,
    all: bool,
) -> (Vec<Row>, Vec<SectionRows>) {
    let mut identity = Vec::new();
    let mut sections = Vec::new();
    let workspace_present = values
        .iter()
        .any(|entry| entry.source.kind() == ConfigValueSourceKind::Workspace);
    for section in ConfigSection::ORDER {
        let mut rows = Vec::new();
        let mut hidden_advanced = 0;
        if *section != ConfigSection::Crews {
            for descriptor in ordered_keys(*section) {
                let Some(entry) = values.iter().find(|entry| entry.key == descriptor.key) else {
                    continue;
                };
                // Even a non-inherited global security setting is configured:
                // keep its explanatory row visible.
                if !all
                    && descriptor.tier == ConfigKeyTier::Advanced
                    && entry.source.kind() == ConfigValueSourceKind::BuiltIn
                    && entry.shadowed_by.is_empty()
                {
                    hidden_advanced += 1;
                    continue;
                }
                let row = Row {
                    label: label_for(*section, &entry.key),
                    value: render_value(&entry.value),
                    state: if is_show_identity_key(&entry.key) {
                        IDENTITY_STATE.to_string()
                    } else {
                        effective_state(entry)
                    },
                    description: described(descriptor),
                    note: shadow_note(entry),
                };
                if is_show_identity_key(&entry.key) {
                    identity.push(row);
                } else {
                    rows.push(row);
                }
            }
        }
        if *section == ConfigSection::Execution {
            rows.push(env_inherit_row(env_inherit));
        }
        let has_crews = *section == ConfigSection::Crews
            && values.iter().any(|entry| entry.key.starts_with("crews."));
        if rows.is_empty() && hidden_advanced == 0 && !has_crews && *section != ConfigSection::Crews
        {
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
            hidden_advanced,
        });
    }
    (identity, sections)
}

fn scoped_rows(
    store: &ConfigStore,
    settings: &[(&'static str, JsonValue)],
    env_inherit: bool,
    all: bool,
) -> (Vec<Row>, Vec<SectionRows>) {
    let mut identity = Vec::new();
    let mut sections = Vec::new();
    for section in ConfigSection::ORDER {
        if *section == ConfigSection::Crews {
            continue;
        }
        let mut rows = Vec::new();
        let mut hidden_advanced = 0;
        for descriptor in ordered_keys(*section) {
            let Some((key, value)) = settings.iter().find(|(key, _)| *key == descriptor.key) else {
                continue;
            };
            if !all && descriptor.tier == ConfigKeyTier::Advanced && !store.is_key_set(key) {
                hidden_advanced += 1;
                continue;
            }
            let row = Row {
                label: label_for(*section, key),
                value: render_value(value),
                state: if is_show_identity_key(key) {
                    IDENTITY_STATE.to_string()
                } else {
                    scoped_state(store, key, value).1.to_string()
                },
                description: described(descriptor),
                note: None,
            };
            if is_show_identity_key(key) {
                identity.push(row);
            } else {
                rows.push(row);
            }
        }
        if *section == ConfigSection::Execution {
            rows.push(env_inherit_row(env_inherit));
        }
        if rows.is_empty() && hidden_advanced == 0 {
            continue;
        }
        sections.push(SectionRows {
            section: *section,
            rows,
            heading_suffix: String::new(),
            hidden_advanced,
        });
    }
    (identity, sections)
}

/// Identity facts lead the text view, outside the Machine key rows.
fn write_identity(out: &mut ConfigShow, layout: &Layout, rows: &[Row]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} written once by orbit init; not settable",
        "Identity"
    );
    for row in rows {
        layout.write_row(out, row);
    }
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

fn write_crews(out: &mut ConfigShow, values: &[EffectiveConfigValue], section: &SectionRows) {
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
    // Disabled crews stay listed; the heading says how many dispatch refuses.
    let disabled = crews.iter().filter(|crew| crew.enabled == "false").count();
    let disabled = if disabled == 0 {
        String::new()
    } else {
        format!(" ({disabled} disabled)")
    };
    let _ = writeln!(
        out,
        "{:<HEADING_WIDTH$} {} defined{disabled}, {origin}",
        section.section.title(),
        crews.len()
    );

    let columns: [&str; 7] = [
        "NAME", "ENABLED", "FROM", "PROVIDER", "MODEL", "EFFORT", "TAGS",
    ];
    let mut widths = columns.map(|column| column.chars().count());
    for crew in &crews {
        for (index, cell) in crew.cells().iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let write_cells = |out: &mut ConfigShow, cells: &[String; 7], trailing: &str| {
        let mut line = String::new();
        for (index, cell) in cells.iter().enumerate() {
            let _ = write!(line, "  {:<width$}", cell, width = widths[index]);
        }
        if let Some(trailing) = trailing.strip_prefix("  ") {
            let _ = write!(out, "{line}  ");
            out.mark_trailing_column();
            let _ = writeln!(out, "{trailing}");
        } else {
            let _ = writeln!(out, "{}", line.trim_end());
        }
    };
    write_cells(out, &columns.map(str::to_string), "  DESCRIPTION");
    for crew in &crews {
        let mut trailing = String::new();
        if !crew.description.is_empty() {
            let _ = write!(trailing, "  {}", crew.description);
        }
        if !crew.env.is_empty() {
            let _ = write!(trailing, "  env {}", crew.env);
        }
        if !crew.annotation.is_empty() {
            let _ = write!(trailing, "  ← {}", crew.annotation);
        }
        write_cells(out, &crew.cells(), &trailing);
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
            // A human crew has no provider or model: it names an operator.
            let human = crew
                .get("kind")
                .is_some_and(|(kind, _)| kind == orbit_types::identity::HUMAN_CREW_KIND);
            if human {
                annotation.push("human crew: operator work, never dispatched");
            }
            CrewRow {
                name,
                enabled: cell("enabled"),
                layer: layers.join("+"),
                provider: if human {
                    orbit_types::identity::HUMAN_CREW_KIND.to_string()
                } else {
                    cell("provider")
                },
                model: cell("model"),
                effort: cell("effort"),
                tags: cell("tags"),
                description: crew
                    .get("description")
                    .map(|(value, _)| value.clone())
                    .filter(|value| value != NO_VALUE)
                    .unwrap_or_default(),
                env: crew
                    .get("env")
                    .map(|(value, _)| value.clone())
                    .unwrap_or_default(),
                annotation: annotation.join(", "),
            }
        })
        .collect()
}

/// The resolved roots and store locations, replacing the old `derived:` block
/// and its single-line `persistence` JSON blob. The rows themselves come from
/// `orbit_core::app::config`, so this view and the dashboard's Paths grid name
/// the same locations.
fn write_paths(out: &mut ConfigShow, rows: &[(String, String)]) {
    let width = rows
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let _ = writeln!(out);
    let _ = writeln!(out, "Paths");
    for (label, value) in rows {
        let _ = write!(out, "  {label:<width$}  ");
        out.mark_trailing_column();
        let _ = writeln!(out, "{value}");
    }
}

/// Display metadata for registered keys and dynamic crew fields.
fn tier_json(key: &str) -> JsonValue {
    json!(
        describe_config_key(key)
            .map_or(ConfigKeyTier::Standard, |entry| entry.tier)
            .token()
    )
}

/// Registry rows for one section, most relevant first.
fn ordered_keys(section: ConfigSection) -> Vec<&'static ConfigKeyDescriptor> {
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

/// The key's accepted values, in the shape `orbit config keys --json` uses.
fn allowed_json(key: &str) -> JsonValue {
    describe_config_key(key).map_or(JsonValue::Null, |descriptor| descriptor.allowed.to_json())
}

/// A row's description with its accepted values appended in brackets, so the
/// text view shows them beside the setting [ORB-22809].
fn described(descriptor: &ConfigKeyDescriptor) -> String {
    match descriptor.allowed.summary() {
        Some(allowed) => format!("{} [{allowed}]", descriptor.description),
        None => descriptor.description.to_string(),
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
fn workspace_suffix(roots: &ConfigShowRoots) -> String {
    roots
        .workspace_binding
        .as_ref()
        .and_then(|binding| binding.repo_root.file_name().map(|name| name.to_owned()))
        .map(|name| format!(" (workspace: {})", name.to_string_lossy()))
        .unwrap_or_default()
}

/// The registry's own delivery values. They are what delivery uses, so a
/// mismatch against `workflow.base_branch` has to be visible here.
fn workspace_binding_line(roots: &ConfigShowRoots) -> Option<String> {
    let binding = roots.workspace_binding.as_ref()?;
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

fn format_value_for_display(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => "null".to_string(),
        other => other.to_string(),
    }
}
