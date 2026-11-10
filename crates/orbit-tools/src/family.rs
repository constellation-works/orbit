//! Tool families: one registered tool per [`ToolFamily`], whose `action`
//! field selects the operation that serves the call.
//!
//! The family table lives in `orbit-types` so policy matching can read it;
//! this module derives each family's schema from its operations' own and
//! resolves a call to the operation it names. A family call is checked here,
//! before any handler runs: the action must be one of the family's, and the
//! fields that action requires must be present, so a refusal names the
//! action's fields rather than a handler's first failure.

use std::collections::BTreeMap;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::protocol::tool_schema::tool_input_schema_for;
use orbit_types::tool::{
    McpIdRouteDeclaration, TOOL_ACTION_FIELD, TOOL_MODE_FIELD, ToolFamily, ToolFamilyAction,
    ToolParam, ToolSchema, tool_family, tool_family_action,
};
use serde_json::{Value, json};

use crate::{Tool, ToolContext, ToolExecutionKind};

/// The selector field's own wording in a family schema.
const WORKSPACE_PARAM: &str = "workspace";

/// What one family says about itself beyond its operations' schemas.
fn family_description(family: &ToolFamily) -> &'static str {
    match family.name {
        "orbit.task.recover" => {
            "Operator recovery for stuck claims, blocked tasks, handoffs and reviews."
        }
        "orbit.run" => "List, show, observe or resume workflow runs.",
        "orbit.workflow" => {
            "Ship tasks, land one, or observe and control the workspace auto-drain."
        }
        "orbit.auto_task" => "List, add, update or mint recurring auto-task definitions.",
        "orbit.decision" => "List, show or raise human decision requests.",
        "orbit.codebase" => {
            "Codebase analytics: ranked rollups, one key's detail, or files changed together."
        }
        "orbit.friction" => "Record a friction, or update one.",
        "orbit.task.artifact" => {
            "Read/attach artifacts; list via `orbit.task.show` (`fields: [\"artifacts\"]`)."
        }
        _ => "",
    }
}

/// One action of a registered family and the operation tool serving it.
#[derive(Clone)]
pub(crate) struct FamilyMember {
    pub(crate) action: &'static ToolFamilyAction,
    pub(crate) schema: ToolSchema,
    pub(crate) execution_kind: ToolExecutionKind,
    pub(crate) id_route: McpIdRouteDeclaration,
}

/// The registered tool of one [`ToolFamily`]. The registry dispatches its
/// calls to the selected operation after [`resolve_family_input`].
pub(crate) struct FamilyTool {
    family: &'static ToolFamily,
    members: Vec<FamilyMember>,
}

impl FamilyTool {
    pub(crate) fn new(family: &'static ToolFamily, members: Vec<FamilyMember>) -> Self {
        Self { family, members }
    }

    fn member(&self, action: &str) -> Option<&FamilyMember> {
        self.members
            .iter()
            .find(|member| member.action.action == action)
    }
}

impl Tool for FamilyTool {
    fn schema(&self) -> ToolSchema {
        family_schema(self.family, &self.members)
    }

    fn input_schema(&self) -> Option<Value> {
        let schema = self.schema();
        let mut input = Value::Object(tool_input_schema_for(&schema.name, &schema.parameters));
        if let Some(action) = input
            .get_mut("properties")
            .and_then(|properties| properties.get_mut(TOOL_ACTION_FIELD))
            .and_then(Value::as_object_mut)
        {
            action.insert(
                "enum".into(),
                json!(
                    self.family
                        .actions
                        .iter()
                        .map(|action| action.action)
                        .collect::<Vec<_>>()
                ),
            );
        }
        Some(input)
    }

    /// The route its id-addressed actions declare. Every action that
    /// addresses one id declares the same route, which the registry checks.
    fn declared_id_route(&self) -> McpIdRouteDeclaration {
        self.members
            .iter()
            .map(|member| member.id_route)
            .find(|route| *route != McpIdRouteDeclaration::Unspecified)
            .unwrap_or_default()
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        if self
            .members
            .iter()
            .all(|member| member.execution_kind == ToolExecutionKind::ReadOnly)
        {
            ToolExecutionKind::ReadOnly
        } else {
            ToolExecutionKind::Mutating
        }
    }

    /// The registry resolves a family call to its operation before
    /// dispatch, so this entry runs only when a caller bypasses it.
    fn execute(&self, _ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        let action = input
            .get(TOOL_ACTION_FIELD)
            .and_then(Value::as_str)
            .and_then(|action| self.member(action));
        Err(OrbitError::Execution(format!(
            "`{}` dispatches through the tool registry{}",
            self.family.name,
            action
                .map(|member| format!(" to `{}`", member.action.action))
                .unwrap_or_default()
        )))
    }
}

/// One family parameter merged across actions: its type, its description by
/// action, and how many actions require it.
type MergedParam<'a> = (String, Vec<(&'a str, String)>, usize);

/// The family schema: `action`, a `mode` when an operation takes its own
/// `action`, then every operation parameter once, in first-declared order.
/// A parameter is required only when every action requires it; the action
/// list names each action's other required fields, and the description of a
/// parameter some actions lack names the actions that take it.
fn family_schema(family: &ToolFamily, members: &[FamilyMember]) -> ToolSchema {
    let all_actions = members.len();
    let action_summary = members
        .iter()
        .map(|member| {
            let required = action_required_fields(member);
            if required.is_empty() {
                member.action.action.to_string()
            } else {
                format!(
                    "{} ({})",
                    member.action.action,
                    required
                        .iter()
                        .map(|(public, _)| public.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    let action_summary = format!("Operation to run, with its required fields: {action_summary}.");
    let mut parameters = vec![ToolParam {
        name: TOOL_ACTION_FIELD.to_string(),
        description: action_summary,
        param_type: "string".to_string(),
        required: true,
    }];

    let mut order: Vec<String> = Vec::new();
    let mut params: BTreeMap<String, MergedParam<'_>> = BTreeMap::new();
    for member in members {
        for param in &member.schema.parameters {
            let public = public_field_name(member.action, &param.name);
            let entry = params.entry(public.clone()).or_insert_with(|| {
                order.push(public.clone());
                (param.param_type.clone(), Vec::new(), 0)
            });
            entry
                .1
                .push((member.action.action, param.description.clone()));
            if param.required {
                entry.2 += 1;
            }
        }
    }
    if let Some(index) = order.iter().position(|name| name == TOOL_MODE_FIELD) {
        let mode = order.remove(index);
        order.insert(0, mode);
    }
    for name in order {
        let Some((param_type, descriptions, required_by)) = params.remove(&name) else {
            continue;
        };
        let description = if name == WORKSPACE_PARAM {
            descriptions
                .first()
                .map(|(_, description)| description.clone())
                .unwrap_or_default()
        } else {
            merged_description(&descriptions, all_actions)
        };
        parameters.push(ToolParam {
            name,
            description,
            param_type,
            required: required_by == all_actions,
        });
    }
    ToolSchema {
        name: family.name.to_string(),
        description: family_description(family).to_string(),
        parameters,
        builtin: true,
    }
}

/// One description for a parameter. Wording every action shares is given
/// once; otherwise each distinct first sentence is led by the actions that
/// use it, so a field that means different things per action says so.
fn merged_description(descriptions: &[(&str, String)], all_actions: usize) -> String {
    let mut groups: Vec<(Vec<&str>, &str)> = Vec::new();
    for (action, description) in descriptions {
        let sentence = first_sentence(description);
        match groups
            .iter_mut()
            .find(|(_, existing)| *existing == sentence)
        {
            Some((actions, _)) => actions.push(action),
            None => groups.push((vec![action], sentence)),
        }
    }
    if let [(_, sentence)] = groups.as_slice()
        && descriptions.len() == all_actions
    {
        return (*sentence).to_string();
    }
    groups
        .into_iter()
        .map(|(actions, sentence)| {
            let sentence = sentence.trim_end_matches('.');
            if sentence.is_empty() {
                format!("For {}", actions.join(", "))
            } else {
                format!("For {}: {sentence}", actions.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
        + "."
}

/// The text up to the first full stop that ends a sentence: one followed by
/// a capital letter, and not closing an abbreviation such as "e.g.".
fn first_sentence(text: &str) -> &str {
    let trimmed = text.trim();
    for (index, _) in trimmed.match_indices(". ") {
        let head = &trimmed[..index];
        let next = trimmed[index + 2..].chars().next();
        let abbreviation = ["e.g", "i.e", "etc", "vs"]
            .iter()
            .any(|short| head.ends_with(short));
        if next.is_some_and(char::is_uppercase) && !abbreviation && !head.ends_with(' ') {
            return &trimmed[..=index];
        }
    }
    trimmed
}

/// The name a family call uses for an operation's input field: an
/// operation's own `action` travels as `mode`.
fn public_field_name(action: &ToolFamilyAction, field: &str) -> String {
    if action.takes_mode && field == TOOL_ACTION_FIELD {
        TOOL_MODE_FIELD.to_string()
    } else {
        field.to_string()
    }
}

/// The fields an action requires, as `(family field, operation field)`. The
/// workspace selector is left to the session and the host, which supply or
/// refuse it with their own message.
fn action_required_fields(member: &FamilyMember) -> Vec<(String, String)> {
    member
        .schema
        .parameters
        .iter()
        .filter(|param| param.required && param.name != WORKSPACE_PARAM)
        .map(|param| {
            (
                public_field_name(member.action, &param.name),
                param.name.clone(),
            )
        })
        .collect()
}

/// A spoke broker sends `orbit.task.artifact.put` a path-free `artifacts`
/// payload that the public schema does not declare; the handler checks it.
pub(crate) fn is_preloaded_artifact_payload(operation: &str, input: &Value) -> bool {
    operation == "orbit.task.artifact.put" && input.get("artifacts").is_some()
}

/// Resolve a call to `family` into its operation and the operation's input:
/// check the action, drop `action` and hand `mode` back as the operation's
/// own `action`. The fields the action requires are checked when its
/// operation runs ([`check_family_action_fields`]), after authorization.
pub(crate) fn resolve_family_input(
    family: &'static ToolFamily,
    members: &[FamilyMember],
    mut input: Value,
) -> Result<(&'static str, Value), OrbitError> {
    let name = family.name;
    let object = input
        .as_object_mut()
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{name}` arguments must be an object")))?;
    let selected = match object.remove(TOOL_ACTION_FIELD) {
        Some(Value::String(action)) => action,
        None | Some(Value::Null) => {
            return Err(OrbitError::InvalidInput(format!(
                "`{name}` requires `{TOOL_ACTION_FIELD}`: one of {}",
                family.action_names()
            )));
        }
        Some(_) => {
            return Err(OrbitError::InvalidInput(format!(
                "`{name}` `{TOOL_ACTION_FIELD}` must be a string: one of {}",
                family.action_names()
            )));
        }
    };
    let selected = selected.trim();
    let Some(member) = members
        .iter()
        .find(|member| member.action.action == selected)
    else {
        let recovery_hint = if name == "orbit.task.artifact" && matches!(selected, "list" | "read")
        {
            "; to list task artifacts, use `orbit.task.show` with `fields: [\"artifacts\"]`"
        } else {
            ""
        };
        return Err(OrbitError::InvalidInput(format!(
            "unknown `{name}` action `{selected}`; expected one of {}{recovery_hint}",
            family.action_names(),
        )));
    };
    let action = member.action;
    // `mode` is the operation's own `action` where it takes one, or an
    // operation field of that name (ship's `pr` or `local`).
    let takes_mode = |member: &FamilyMember| {
        member.action.takes_mode
            || member
                .schema
                .parameters
                .iter()
                .any(|param| param.name == TOOL_MODE_FIELD)
    };
    if object.contains_key(TOOL_MODE_FIELD) && !takes_mode(member) {
        let takes = members
            .iter()
            .filter(|member| takes_mode(member))
            .map(|member| member.action.action)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OrbitError::InvalidInput(format!(
            "`{name}` action `{selected}` takes no `{TOOL_MODE_FIELD}`; only {takes} does"
        )));
    }
    if action.takes_mode
        && let Some(mode) = object.remove(TOOL_MODE_FIELD)
    {
        object.insert(TOOL_ACTION_FIELD.to_string(), mode);
    }
    Ok((action.operation, input))
}

/// Refuse a family action call missing a field its action requires, naming
/// every required field and those missing. A field counts as supplied under
/// its name or any declared alias (`aliases` pairs a field with one alias).
/// A missing artifact path or source also carries the operation's remedy.
pub(crate) fn check_family_action_fields(
    family: &'static ToolFamily,
    member: &FamilyMember,
    input: &Value,
    aliases: &[(&str, &str)],
    ctx: &ToolContext,
) -> Result<(), OrbitError> {
    let operation = member.action.operation;
    if is_preloaded_artifact_payload(operation, input) {
        return Ok(());
    }
    let supplied = |key: &str| {
        input
            .get(key)
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
    };
    let required = action_required_fields(member);
    let missing = required
        .iter()
        .filter(|(_, field)| {
            !supplied(field)
                && !aliases
                    .iter()
                    .any(|(name, alias)| name == field && supplied(alias))
        })
        .map(|(_, field)| field.as_str())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let quoted = |fields: Vec<&str>| {
        fields
            .iter()
            .map(|field| format!("`{field}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let remedy = match (operation, missing.as_slice()) {
        ("orbit.task.artifact.put", fields) if fields.contains(&"source_path") => {
            format!("; {}", crate::artifact_materialize::SOURCE_PATH_REMEDY)
        }
        ("orbit.task.artifact.get", fields) if fields.contains(&"path") => format!(
            "; {}",
            orbit_common::governance::task::artifact_listing_hint(&ctx.session_context)
        ),
        _ => String::new(),
    };
    let public = |field: &str| public_field_name(member.action, field);
    let missing_public = missing
        .iter()
        .map(|field| public(field))
        .collect::<Vec<_>>();
    let required_public = required
        .iter()
        .map(|(public, _)| public.clone())
        .collect::<Vec<_>>();
    Err(OrbitError::InvalidInput(format!(
        "`{}` action `{}` requires {}; missing {}{remedy}",
        family.name,
        member.action.action,
        quoted(required_public.iter().map(String::as_str).collect()),
        quoted(missing_public.iter().map(String::as_str).collect()),
    )))
}

/// The refusal for a call naming an operation instead of its family, or a
/// `family:action` name instead of the family call: neither is a tool, and
/// both are answered with the family call that reaches the operation.
pub(crate) fn operation_is_not_a_tool(operation: &str) -> Option<OrbitError> {
    let (family, action) = tool_family_action(operation)?;
    Some(OrbitError::not_found_with_guidance(
        orbit_common::NotFoundKind::Tool,
        operation,
        format!(" (call {} with action \"{}\")", family.name, action.action),
    ))
}

/// Whether `name` is a registered family.
pub(crate) fn is_family(name: &str) -> bool {
    tool_family(name).is_some()
}

/// Build one family's members from the registered operation tools.
pub(crate) fn family_members(
    family: &'static ToolFamily,
    lookup: impl Fn(&str) -> Option<Arc<dyn Tool>>,
) -> Option<Vec<FamilyMember>> {
    family
        .actions
        .iter()
        .map(|action| {
            let tool = lookup(action.operation)?;
            Some(FamilyMember {
                action,
                schema: tool.schema(),
                execution_kind: tool.execution_kind(),
                id_route: tool.declared_id_route(),
            })
        })
        .collect()
}
