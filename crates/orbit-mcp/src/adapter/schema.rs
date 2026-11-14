use std::sync::Arc;

use orbit_common::governance::friction::{DEFAULT_FRICTION_TAGS, FRICTION_TITLE_MAX_CHARS};
use orbit_common::protocol::tool_schema::tool_input_schema_for;
use orbit_types::tool::{
    McpIdRoute, McpInputDecoration, McpToolAnnotations, McpToolDefinition, McpToolScope,
    TOOL_ACTION_FIELD, ToolParam, ToolSchema, tool_family,
};
use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};

use super::name_map::sanitize_tool_name;

pub(super) fn schema_to_tool(
    schema: ToolSchema,
    input_schema: JsonObject,
    annotations: Option<McpToolAnnotations>,
) -> Tool {
    let description = schema.description.clone();
    let advertised_name = sanitize_tool_name(&schema.name);
    let tool = Tool::new(advertised_name, description, Arc::new(input_schema));
    match annotations {
        Some(annotations) => tool.with_annotations(tool_annotations(annotations)),
        None => tool,
    }
}

/// The rmcp hints for one definition; an unset hint stays unadvertised.
fn tool_annotations(annotations: McpToolAnnotations) -> ToolAnnotations {
    let mut hints = ToolAnnotations::new();
    hints.read_only_hint = annotations.read_only;
    hints.destructive_hint = annotations.destructive;
    hints.idempotent_hint = annotations.idempotent;
    hints.open_world_hint = annotations.open_world;
    hints
}

/// Canonical name of the authoritative server's workspace selector.
pub(crate) const WORKSPACE_SELECTOR_PARAM: &str = "workspace";

/// One description for every advertised `workspace` selector.
///
/// Required versus optional stays in the schema `required` array. Routing
/// rules live in the installed Orbit skill `references/task-fields.md`.
pub(crate) const WORKSPACE_SELECTOR_DESCRIPTION: &str =
    "Workspace selector from orbit_workspace_list.";

/// Property descriptions on these tools are hand-written one-sentence contract.
const HAND_AUTHORED_PROPERTY_TOOLS: &[&str] =
    &["orbit.task.add", "orbit.task.update", "orbit.task.list"];
const HAND_AUTHORED_PROPERTY_CAP: usize = 200;
/// Other property descriptions stay short so `tools/list` cannot regrow.
const ADVERTISED_PROPERTY_CAP: usize = 48;
/// Tool descriptions that tests and the skill link require whole.
const PRESERVED_TOOL_DESCRIPTIONS: &[&str] = &[
    "orbit.task.add",
    "orbit.task.update",
    "orbit.task.list",
    "orbit.task.show",
];
const ADVERTISED_TOOL_DESCRIPTION_CAP: usize = 80;

/// A tool's own declared input schema, as advertised before the host adds its
/// selector: every keyword verbatim except the root `type`, which is always
/// `"object"` — MCP's `inputSchema` must describe an object and `tools/call`
/// arguments always are one (design §4.2).
pub(super) fn declared_input_schema(declared: &JsonObject) -> JsonObject {
    let mut schema = declared.clone();
    schema.insert("type".to_string(), json!("object"));
    schema
}

/// Whether the session being served already carries a workspace selector.
///
/// The two states are advertised differently because they place different
/// obligations on the caller: a bound session may omit the selector, an
/// unbound one is refused without it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WorkspaceBinding {
    Bound,
    Unbound,
}

/// How this session advertises the workspace selector on workspace-scoped tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SelectorAdvertisement {
    /// v1 local/remote MCP: registered name, `ws_*`, or absolute path.
    Authoritative(WorkspaceBinding),
    /// Federated mux: copy `selector` from federated `orbit.workspace.list`.
    Federated,
}

/// Advertise the workspace selector on every workspace-scoped tool.
pub(super) fn ensure_workspace_selector(
    schema: &mut JsonObject,
    definition: &McpToolDefinition,
    advertisement: SelectorAdvertisement,
) {
    if definition.scope != McpToolScope::WorkspaceRequired {
        return;
    }
    let id_routed = definition.rules.id_route == McpIdRoute::Task;
    match advertisement {
        SelectorAdvertisement::Authoritative(binding) => {
            ensure_authoritative_selector(schema, definition, binding, id_routed);
        }
        SelectorAdvertisement::Federated => {
            ensure_federated_selector(schema, id_routed, definition.rules.workspace_optional)
        }
    }
}

/// The selector is a routing argument only when the plugin did not declare it
/// as part of its own input. Built-in tools retain their existing inputs.
pub(super) fn host_owns_plugin_selector(definition: &McpToolDefinition) -> bool {
    !definition.schema.builtin
        && definition.scope == McpToolScope::WorkspaceRequired
        && !definition
            .schema
            .parameters
            .iter()
            .any(|parameter| parameter.name == WORKSPACE_SELECTOR_PARAM)
}

fn ensure_authoritative_selector(
    schema: &mut JsonObject,
    definition: &McpToolDefinition,
    binding: WorkspaceBinding,
    id_routed: bool,
) {
    let Some(properties) = selector_properties(schema) else {
        return;
    };
    // A declared filter keeps its required-ness. Every other path still
    // inserts the selector when the tool did not declare one. The description
    // is the same line either way.
    if definition.rules.declared_workspace_selector {
        set_workspace_description(properties);
        return;
    }
    if definition.rules.workspace_optional {
        properties.insert(
            WORKSPACE_SELECTOR_PARAM.to_string(),
            json!({
                "type": "string",
                "description": WORKSPACE_SELECTOR_DESCRIPTION,
            }),
        );
        unrequire_property(schema, WORKSPACE_SELECTOR_PARAM);
        return;
    }
    if !properties.contains_key(WORKSPACE_SELECTOR_PARAM) {
        properties.insert(
            WORKSPACE_SELECTOR_PARAM.to_string(),
            json!({
                "type": "string",
                "description": WORKSPACE_SELECTOR_DESCRIPTION,
            }),
        );
    } else {
        set_workspace_description(properties);
    }
    if binding == WorkspaceBinding::Unbound && !id_routed {
        require_property(schema, WORKSPACE_SELECTOR_PARAM);
    }
}

fn set_workspace_description(properties: &mut JsonObject) {
    if let Some(property) = properties
        .get_mut(WORKSPACE_SELECTOR_PARAM)
        .and_then(Value::as_object_mut)
    {
        property.insert(
            "description".to_string(),
            json!(WORKSPACE_SELECTOR_DESCRIPTION),
        );
    }
}

fn ensure_federated_selector(schema: &mut JsonObject, id_routed: bool, workspace_optional: bool) {
    let Some(properties) = selector_properties(schema) else {
        return;
    };
    // Replace any v1 local wording a tool declared itself. Required-ness
    // still follows the route: an id-routed or workspace-optional tool may
    // omit the selector. The description is the shared line.
    properties.insert(
        WORKSPACE_SELECTOR_PARAM.to_string(),
        json!({
            "type": "string",
            "description": WORKSPACE_SELECTOR_DESCRIPTION,
        }),
    );
    if id_routed || workspace_optional {
        // A tool that declared its own selector as required is still
        // addressable by id here.
        unrequire_property(schema, WORKSPACE_SELECTOR_PARAM);
    } else {
        require_property(schema, WORKSPACE_SELECTOR_PARAM);
    }
}

/// The `properties` map the selector joins. A declared schema may name no
/// properties at all; it still needs the selector to be callable.
fn selector_properties(schema: &mut JsonObject) -> Option<&mut JsonObject> {
    schema
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
}

fn require_property(schema: &mut JsonObject, property: &str) {
    let required = schema.entry("required").or_insert_with(|| json!([]));
    if let Some(required) = required.as_array_mut()
        && !required.iter().any(|name| name == property)
    {
        required.push(json!(property));
    }
}

fn unrequire_property(schema: &mut JsonObject, property: &str) {
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|name| name != property);
    }
}

pub(super) fn build_input_schema(tool_name: &str, params: &[ToolParam]) -> JsonObject {
    tool_input_schema_for(tool_name, params)
}

pub(super) fn apply_input_decoration(
    schema: &mut JsonObject,
    decoration: McpInputDecoration,
    taxonomy: Option<&[(String, String)]>,
) {
    if decoration == McpInputDecoration::Friction {
        decorate_friction_schema(schema, taxonomy);
    }
    if decoration == McpInputDecoration::Search {
        if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            if let Some(query) = properties.get_mut("query").and_then(Value::as_object_mut) {
                query.insert("minLength".to_string(), json!(1));
            }
            if let Some(tag) = properties.get_mut("tag").and_then(Value::as_object_mut)
                && tag.get("type").and_then(Value::as_str) == Some("array")
            {
                tag.insert("minItems".to_string(), json!(1));
            }
        }
        // A query or tag narrows any search; `kind: friction` alone lists
        // friction records.
        schema.insert(
            "allOf".to_string(),
            json!([{ "anyOf": [
                { "required": ["query"] },
                { "required": ["tag"] },
                { "required": ["kind"], "properties": { "kind": { "const": "friction" } } }
            ] }]),
        );
    }
}

fn decorate_friction_schema(schema: &mut JsonObject, taxonomy: Option<&[(String, String)]>) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };

    if let Some(title) = properties.get_mut("title").and_then(Value::as_object_mut) {
        title.insert("maxLength".to_string(), json!(FRICTION_TITLE_MAX_CHARS));
    }

    let Some(tags) = properties.get_mut("tags").and_then(Value::as_object_mut) else {
        return;
    };
    let fallback;
    let entries = match taxonomy.filter(|entries| !entries.is_empty()) {
        Some(entries) => entries,
        None => {
            fallback = DEFAULT_FRICTION_TAGS
                .iter()
                .map(|(tag, description)| ((*tag).to_string(), (*description).to_string()))
                .collect::<Vec<_>>();
            &fallback
        }
    };
    let values = entries
        .iter()
        .map(|(tag, _description)| tag.clone())
        .collect::<Vec<_>>();
    if tags.get("type").and_then(Value::as_str) == Some("array")
        && let Some(items) = tags.get_mut("items").and_then(Value::as_object_mut)
    {
        items.insert("enum".to_string(), json!(values));
    }
    // The vocabulary and its workspace override live in the installed skill,
    // not in the advertised schema.
    tags.insert(
        "description".to_string(),
        Value::String("Friction taxonomy tags.".to_string()),
    );
}

/// Shorten advertised property descriptions after workspace selection.
///
/// `orbit.task.add`, `orbit.task.update` and `orbit.task.list` keep the
/// one-sentence descriptions written on the operation. Every other tool is
/// capped so the operator `tools/list` stays inside its byte budget.
pub(super) fn slim_advertised_property_descriptions(schema: &mut JsonObject, tool_name: &str) {
    let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) else {
        return;
    };
    let hand_authored = HAND_AUTHORED_PROPERTY_TOOLS.contains(&tool_name);
    // A family's descriptions are already one sentence each, and its
    // `action` list names every action's required fields, so neither is cut.
    let family = tool_family(tool_name).is_some();
    for (name, property) in properties.iter_mut() {
        if name == WORKSPACE_SELECTOR_PARAM || (family && name == TOOL_ACTION_FIELD) {
            continue;
        }
        let Some(object) = property.as_object_mut() else {
            continue;
        };
        let Some(description) = object.get("description").and_then(Value::as_str) else {
            continue;
        };
        let slim = if hand_authored || family {
            cap_description(description, HAND_AUTHORED_PROPERTY_CAP)
        } else {
            advertised_other_property_description(description)
        };
        if slim != description {
            object.insert("description".to_string(), Value::String(slim));
        }
    }
}

/// First complete sentence or clause, for tools other than preserved writes.
///
/// A shortened tool description keeps the source's docs pointer when it has
/// one, so detail moved into installed references still reaches agents.
pub(super) fn advertised_tool_description(tool_name: &str, description: &str) -> String {
    if PRESERVED_TOOL_DESCRIPTIONS.contains(&tool_name) {
        return description.to_string();
    }
    let shortened = cap_description(description, ADVERTISED_TOOL_DESCRIPTION_CAP);
    if shortened.contains("references/") {
        return shortened;
    }
    match docs_pointer_sentence(description) {
        Some(pointer) if !shortened.contains(&pointer) => {
            let separator = if shortened.ends_with('.') { " " } else { ". " };
            format!("{shortened}{separator}{pointer}")
        }
        _ => shortened,
    }
}

fn advertised_other_property_description(description: &str) -> String {
    let sentence = first_sentence(description);
    let lower = sentence.to_ascii_lowercase();
    if lower.contains("string or array")
        || lower.contains("string or an array")
        || lower.contains("comma-separated")
    {
        return "Array of strings.".to_string();
    }
    cap_description(description, ADVERTISED_PROPERTY_CAP)
}

/// The sentence carrying a docs pointer (`references/…` or an installed
/// skill), when the source has one.
fn docs_pointer_sentence(text: &str) -> Option<String> {
    split_sentences(text).into_iter().find(|sentence| {
        sentence.contains("references/")
            || sentence.contains("installed Orbit skill")
            || sentence.contains("installed skill")
    })
}

fn first_sentence(text: &str) -> &str {
    let trimmed = text.trim();
    split_sentences(trimmed)
        .into_iter()
        .next()
        .map(|sentence| {
            let start = trimmed.find(&sentence).unwrap_or(0);
            &trimmed[start..start + sentence.len()]
        })
        .unwrap_or(trimmed)
}

/// Split on sentence-ending periods: a `.` followed by an uppercase letter,
/// a digit or a backtick, so dotted names and abbreviations stay whole.
fn split_sentences(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let bytes = trimmed.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'.' {
            let mut next = index + 1;
            while next < bytes.len() && bytes[next] == b' ' {
                next += 1;
            }
            if next > index + 1
                && next < bytes.len()
                && (bytes[next].is_ascii_uppercase()
                    || bytes[next].is_ascii_digit()
                    || bytes[next] == b'`')
            {
                let sentence = trimmed[start..=index].trim();
                if !sentence.is_empty() {
                    sentences.push(sentence.to_string());
                }
                start = next;
                index = next;
                continue;
            }
        }
        index += 1;
    }
    let tail = trimmed[start..].trim();
    if !tail.is_empty() {
        sentences.push(tail.to_string());
    }
    sentences
}

/// Words a complete sentence or clause never ends with.
const DANGLING_END_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "nor", "by", "with", "without", "within", "for", "to",
    "of", "in", "on", "at", "from", "as", "per", "via", "over", "under", "into", "onto", "between",
    "through", "during", "before", "after", "about", "against", "whose", "which", "that", "this",
    "is", "are", "was", "were", "be", "been", "has", "have", "had", "e.g", "i.e",
];

fn is_complete_candidate(head: &str) -> bool {
    let candidate = head.trim().trim_end_matches([',', ';', ':']);
    if candidate.chars().count() < 10 {
        return false;
    }
    if !candidate.matches('`').count().is_multiple_of(2) {
        return false;
    }
    let opens = candidate.matches('(').count();
    let closes = candidate.matches(')').count();
    if opens != closes {
        return false;
    }
    let last_word = candidate
        .rsplit(|ch: char| ch.is_whitespace())
        .next()
        .unwrap_or("")
        .trim_matches(|ch: char| "`\"'().,;:".contains(ch))
        .to_ascii_lowercase();
    !last_word.is_empty() && !DANGLING_END_WORDS.contains(&last_word.as_str())
}

/// A clause cut is only valid when it does not slice a value list: a comma
/// after a `:` list introduction is part of the list, not a clause boundary.
fn clause_candidate(head: &str, delimiter: char) -> Option<String> {
    // A head that already contains a comma is likely a value list in
    // progress; cutting at its next comma would present a partial list.
    if delimiter == ',' && (head.contains(": ") || head.contains(',')) {
        return None;
    }
    if !is_complete_candidate(head) {
        return None;
    }
    let candidate = head.trim().trim_end_matches([',', ';', ':']);
    if candidate.ends_with('.') {
        Some(candidate.to_string())
    } else {
        Some(format!("{candidate}."))
    }
}

/// Clause boundaries in `text`, as (char count of the head, delimiter, head).
fn clause_boundaries(text: &str) -> Vec<(usize, char, &str)> {
    let mut boundaries = Vec::new();
    let mut char_count = 0usize;
    for (byte_index, ch) in text.char_indices() {
        char_count += 1;
        if matches!(ch, ';' | ',' | ':') {
            boundaries.push((char_count - 1, ch, &text[..byte_index]));
        } else if ch == '(' && byte_index > 0 && text[..byte_index].ends_with(' ') {
            boundaries.push((char_count - 1, '(', &text[..byte_index]));
        }
    }
    boundaries
}

/// Shorten to a complete sentence or clause. The cap is a preference, never
/// a licence to cut mid-clause: when no clause ends within it, the first
/// clause or sentence beyond it is kept whole.
fn cap_description(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let Some(first) = split_sentences(trimmed).into_iter().next() else {
        return trimmed.to_string();
    };
    if first.chars().count() <= max_chars {
        return first;
    }
    let boundaries = clause_boundaries(&first);
    // Strong boundaries first (`;`, then `:`), then commas, within the cap.
    for wanted in [';', ':', ','] {
        if let Some(candidate) = boundaries
            .iter()
            .filter(|(count, delimiter, _)| *count <= max_chars && *delimiter == wanted)
            .rev()
            .find_map(|(_, delimiter, head)| clause_candidate(head, *delimiter))
        {
            return candidate;
        }
    }
    // No safe boundary within the cap: keep the first complete clause beyond
    // it rather than inventing a sentence by truncating mid-clause.
    if let Some(candidate) = boundaries
        .iter()
        .filter(|(count, _, _)| *count > max_chars)
        .find_map(|(_, delimiter, head)| clause_candidate(head, *delimiter))
    {
        return candidate;
    }
    first
}
