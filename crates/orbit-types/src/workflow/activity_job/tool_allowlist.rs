use std::borrow::Cow;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::activity_v2::{ActivityV2, ActivityV2Spec, AgentLoopSpec};
use crate::tool::{TOOL_ACTION_SEPARATOR, tool_call_name, tool_family, tool_family_action};

/// Explicit list of permitted wildcard roots (§6 / §12 Q7).
///
/// Any wildcard must have its root prefix on this list.
/// Top-level `orbit.*` is deliberately NOT permitted — the max-depth-2 rule
/// the design doc mentions is implemented here as an explicit allowlist so the
/// set grows deliberately and every reviewer sees the full scope.
pub const V2_TOOL_WILDCARD_ROOTS: &[&str] = &[
    "orbit.friction.",
    "orbit.task.",
    "orbit.semantic.",
    // Reserved for audit-session tools. No builtin tools currently live under
    // this root, so registry validation treats it as intentionally empty.
    "orbit.audit.",
    "fs.",
    "proc.",
];

pub const V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS: &[&str] = &["orbit.audit."];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolAllowlistError {
    #[error(
        "wildcard root not permitted in allowlist entry `{entry}` (see V2_TOOL_WILDCARD_ROOTS)"
    )]
    WildcardRootNotPermitted { entry: String },
    #[error("empty tool name in allowlist entry at index {index}")]
    EmptyName { index: usize },
    #[error("unknown tool name in allowlist entry `{entry}`")]
    UnknownToolName { entry: String },
    #[error("wildcard allowlist entry `{entry}` did not match any registered tools")]
    WildcardRootMatchesNoTools { entry: String },
    #[error(
        "allowlist entry `{entry}` grants `proc.spawn` but the activity omits a program policy; declare `proc_allowed_programs` (write `proc_allowed_programs: []` to deny every program) or `proc_disallowed_programs`"
    )]
    ProcSpawnWithoutProgramAllowlist { entry: String },
    #[error(
        "`tools` and `tool_disallow_list` are mutually exclusive; declare an explicit `tools` allowlist or a `tool_disallow_list`, not both"
    )]
    ToolsAndDisallowListBothSet,
    #[error("`tool_disallow_list` invalid: {source}")]
    DisallowList { source: Box<ToolAllowlistError> },
    #[error(
        "`tool_disallow_list` leaves `proc.spawn` callable but the activity omits a program policy; declare `proc_allowed_programs` (write `proc_allowed_programs: []` to deny every program), `proc_disallowed_programs`, or disallow `proc.spawn`"
    )]
    ProcSpawnNotDisallowedWithoutProgramAllowlist,
    #[error("`proc_allowed_programs` and `proc_disallowed_programs` are mutually exclusive")]
    BothProgramListsSet,
}

/// How an `agent_loop` activity bounds the Orbit tools its agent may call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityToolPolicyMode {
    /// `tools` is the exact allowlist (the legacy and custom-activity mode).
    Allow,
    /// Every registered agent-facing tool except `tool_disallow_list`.
    Deny,
}

impl ActivityToolPolicyMode {
    /// The stable token a managed run and its audit record carry.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

impl AgentLoopSpec {
    /// The tool policy this activity declares. Declaring
    /// `tool_disallow_list` at all selects deny mode.
    pub fn tool_policy_mode(&self) -> ActivityToolPolicyMode {
        if self.tool_disallow_list.is_some() {
            ActivityToolPolicyMode::Deny
        } else {
            ActivityToolPolicyMode::Allow
        }
    }
}

/// A deny-mode activity's disallow list, carried to the enforcement point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityToolDenyPolicy {
    /// Activity whose `tool_disallow_list` this is.
    pub activity: String,
    /// Disallowed concrete names and permitted wildcard roots.
    pub disallow_list: Vec<String>,
}

impl ActivityToolDenyPolicy {
    /// Does the disallow list cover `tool_name`?
    pub fn denies(&self, tool_name: &str) -> bool {
        tool_allowed(tool_name, &self.disallow_list)
    }

    /// The refusal a denied call surfaces.
    pub fn denial_message(&self, tool_name: &str) -> String {
        format!(
            "tool '{}' is in the activity disallow list ({})",
            tool_call_name(tool_name),
            self.activity
        )
    }
}

/// Deny mode's concrete callable set: `registered_tools` minus every tool the
/// disallow list covers, in registry order and deduplicated. A family the
/// list covers only partly contributes its remaining `family:action` names.
///
/// A deny-mode run also stamps this set as its legacy `ORBIT_ACTIVITY_TOOLS`
/// allowlist, so an MCP server that predates deny mode enforces an equivalent
/// exact list instead of reading an empty one as unrestricted.
pub fn tools_allowed_by_disallow_list<'a, I>(
    disallow_list: &[String],
    registered_tools: I,
) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    registered_tools
        .into_iter()
        .flat_map(|tool| callable_names(tool, |name| !tool_allowed(name, disallow_list)))
        .filter(|tool| seen.insert(tool.to_string()))
        .map(Cow::into_owned)
        .collect()
}

/// An `agent_loop` activity that declares neither tools nor a disallow list.
///
/// Its enforcement is ambiguous today — the schema reads the empty allowlist
/// as "no tools" while managed dispatch has always read it as unrestricted —
/// so asset load warns instead of silently choosing. [ORB-13315]
pub fn activity_tool_policy_deprecation(activity: &ActivityV2) -> Option<&'static str> {
    let spec = agent_loop_spec(activity)?;
    (spec.tools.is_empty() && spec.tool_disallow_list.is_none()).then_some(
        "declares an empty `tools:` list, whose enforcement is deprecated; declare explicit `tools` or a `tool_disallow_list`",
    )
}

/// Tool whose activity-layer program allowlist must be declared explicitly.
const PROC_SPAWN_TOOL: &str = "proc.spawn";

/// Validate an activity's declared tool allowlist at asset-load time.
/// Returns Ok(()) when every entry is a concrete tool name or a permitted
/// wildcard root. Does NOT verify that concrete tool names resolve in the
/// registry — that's a separate load-time check at the engine layer.
pub fn validate_tool_allowlist(allowlist: &[String]) -> Result<(), ToolAllowlistError> {
    for (index, entry) in allowlist.iter().enumerate() {
        if entry.trim().is_empty() {
            return Err(ToolAllowlistError::EmptyName { index });
        }
        if let Some(prefix) = wildcard_prefix(entry)
            && !V2_TOOL_WILDCARD_ROOTS.contains(&prefix)
        {
            return Err(ToolAllowlistError::WildcardRootNotPermitted {
                entry: entry.clone(),
            });
        }
    }
    Ok(())
}

/// Validate an activity allowlist against the registered tool surface.
///
/// Concrete names must resolve exactly; a `family:action` name needs its
/// family registered and the action known. Wildcards must use an approved root,
/// and must expand to at least one registered tool unless the root is an
/// explicitly documented empty reservation.
pub fn validate_tool_allowlist_against_registered_tools<'a, I>(
    allowlist: &[String],
    registered_tools: I,
) -> Result<(), ToolAllowlistError>
where
    I: IntoIterator<Item = &'a str>,
{
    validate_tool_allowlist(allowlist)?;

    let registered_tools: BTreeSet<&str> = registered_tools.into_iter().collect();
    for entry in allowlist {
        if let Some(prefix) = wildcard_prefix(entry) {
            let has_match = registered_tools.iter().any(|tool| tool.starts_with(prefix));
            if !has_match && !V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS.contains(&prefix) {
                return Err(ToolAllowlistError::WildcardRootMatchesNoTools {
                    entry: entry.clone(),
                });
            }
            continue;
        }

        let registered = match entry.split_once(TOOL_ACTION_SEPARATOR) {
            Some((family, _)) => {
                tool_family_action(entry).is_some() && registered_tools.contains(family)
            }
            None => registered_tools.contains(entry.as_str()),
        };
        if !registered {
            return Err(ToolAllowlistError::UnknownToolName {
                entry: entry.clone(),
            });
        }
    }

    Ok(())
}

pub fn validate_activity_tool_allowlist(activity: &ActivityV2) -> Result<(), ToolAllowlistError> {
    let Some(spec) = agent_loop_spec(activity) else {
        return Ok(());
    };
    reject_both_tool_lists(spec)?;
    validate_tool_allowlist(&spec.tools)?;
    if let Some(disallow_list) = &spec.tool_disallow_list {
        validate_tool_allowlist(disallow_list).map_err(disallow_list_error)?;
    }
    validate_proc_spawn_program_allowlist(spec)
}

pub fn validate_activity_tool_allowlist_against_registered_tools<'a, I>(
    activity: &ActivityV2,
    registered_tools: I,
) -> Result<(), ToolAllowlistError>
where
    I: IntoIterator<Item = &'a str>,
{
    let Some(spec) = agent_loop_spec(activity) else {
        return Ok(());
    };
    reject_both_tool_lists(spec)?;
    let registered_tools: Vec<&str> = registered_tools.into_iter().collect();
    validate_tool_allowlist_against_registered_tools(
        &spec.tools,
        registered_tools.iter().copied(),
    )?;
    if let Some(disallow_list) = &spec.tool_disallow_list {
        validate_tool_allowlist_against_registered_tools(disallow_list, registered_tools)
            .map_err(disallow_list_error)?;
    }
    validate_proc_spawn_program_allowlist(spec)
}

fn reject_both_tool_lists(spec: &AgentLoopSpec) -> Result<(), ToolAllowlistError> {
    if spec.tool_disallow_list.is_some() && !spec.tools.is_empty() {
        return Err(ToolAllowlistError::ToolsAndDisallowListBothSet);
    }
    Ok(())
}

fn disallow_list_error(source: ToolAllowlistError) -> ToolAllowlistError {
    ToolAllowlistError::DisallowList {
        source: Box::new(source),
    }
}

/// An activity that grants `proc.spawn` must declare one program policy.
/// Omitting the key once meant "unconstrained", which
/// made the safer-looking asset the more permissive one: an explicit `[]`
/// denied every program while the absent key allowed all of them. Requiring
/// the pairing at load time keeps the control fail-closed — deny-all is
/// something an author opts into by writing `[]`, not something they lose by
/// forgetting a key. [ORB-10959]
fn validate_proc_spawn_program_allowlist(spec: &AgentLoopSpec) -> Result<(), ToolAllowlistError> {
    if spec.proc_allowed_programs.is_some() && spec.proc_disallowed_programs.is_some() {
        return Err(ToolAllowlistError::BothProgramListsSet);
    }
    if spec.proc_allowed_programs.is_some() || spec.proc_disallowed_programs.is_some() {
        return Ok(());
    }
    if let Some(disallow_list) = &spec.tool_disallow_list {
        // Deny mode grants `proc.spawn` unless the disallow list covers it.
        return if tool_allowed(PROC_SPAWN_TOOL, disallow_list) {
            Ok(())
        } else {
            Err(ToolAllowlistError::ProcSpawnNotDisallowedWithoutProgramAllowlist)
        };
    }
    match proc_spawn_grant(&spec.tools) {
        Some(entry) => Err(ToolAllowlistError::ProcSpawnWithoutProgramAllowlist {
            entry: entry.to_string(),
        }),
        None => Ok(()),
    }
}

/// The allowlist entry that grants `proc.spawn`, named so the load error points
/// at the concrete name or the wildcard root that covers it.
fn proc_spawn_grant(allowlist: &[String]) -> Option<&str> {
    allowlist
        .iter()
        .find(|entry| entry_matches(entry, PROC_SPAWN_TOOL))
        .map(String::as_str)
}

/// Runtime check: is `tool_name` permitted by `allowlist`?
/// Empty allowlist means nothing is allowed (explicit policy per §6.1).
///
/// `tool_name` is a tool, a family action (`orbit.decision:raise`) or the
/// operation serving one (`orbit.decision.raise`). A family action is covered
/// by its family or by its own `family:action` entry; a bare family only by
/// an entry covering every action.
pub fn tool_allowed(tool_name: &str, allowlist: &[String]) -> bool {
    if let Some(family) = tool_family(tool_name) {
        return family.actions.iter().all(|action| {
            allowlist
                .iter()
                .any(|entry| entry_matches(entry, action.operation))
        });
    }
    allowlist
        .iter()
        .any(|entry| entry_matches(entry, tool_name))
}

/// Does one allowlist entry — a concrete name, a `family:action` name or a
/// wildcard root — cover `tool_name`?
fn entry_matches(entry: &str, tool_name: &str) -> bool {
    let wildcard =
        |name: &str| wildcard_prefix(entry).is_some_and(|prefix| name.starts_with(prefix));
    match tool_family_action(tool_name) {
        Some((family, action)) => {
            entry == family.name
                || entry == family.qualified_action(action.action)
                || wildcard(family.name)
                || wildcard(action.operation)
        }
        None => entry == tool_name || wildcard(tool_name),
    }
}

/// The policy names a registered tool contributes to an exact callable set:
/// itself, or for a family whose actions `included` admits only partly,
/// one `family:action` name per included action.
fn callable_names<'a>(tool: &'a str, included: impl Fn(&str) -> bool) -> Vec<Cow<'a, str>> {
    let Some(family) = tool_family(tool) else {
        return if included(tool) {
            vec![Cow::Borrowed(tool)]
        } else {
            Vec::new()
        };
    };
    let actions = family
        .actions
        .iter()
        .map(|action| family.qualified_action(action.action))
        .filter(|name| included(name))
        .collect::<Vec<_>>();
    if actions.len() == family.actions.len() {
        vec![Cow::Borrowed(tool)]
    } else {
        actions.into_iter().map(Cow::Owned).collect()
    }
}

fn agent_loop_spec(activity: &ActivityV2) -> Option<&AgentLoopSpec> {
    match &activity.spec {
        ActivityV2Spec::AgentLoop(spec) => Some(spec),
        ActivityV2Spec::Deterministic(_) => None,
    }
}

fn wildcard_prefix(entry: &str) -> Option<&str> {
    entry.strip_suffix('*')
}
