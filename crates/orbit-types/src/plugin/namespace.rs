//! Namespace rules for plugin tool names.
//!
//! A plugin owns `<ns>.*` (MCP `<ns>_*`). `orbit.<ns>.*` is reserved for
//! Orbit-originated plugins and is only honoured when the loader verifies the
//! first-party origin. Every built-in CLI command name is reserved so a
//! future derived `orbit <ns> <verb>` group cannot shadow one.

/// Prefix of the reserved first-party tool namespace.
pub const ORBIT_NAMESPACE_PREFIX: &str = "orbit";

/// Publisher whose plugins may claim `orbit.<ns>.*` (with `origin: orbit`).
pub const FIRST_PARTY_PUBLISHER: &str = "constellation-works";

/// Top-level `orbit <command>` names, including hidden compatibility
/// commands. A plugin namespace equal to one of these is refused.
///
/// This list is pinned to the CLI's clap tree by a test in `orbit-cli`
/// (`command::tests::plugin`), which is the one place that can see both.
pub const RESERVED_CLI_COMMANDS: &[&str] = &[
    "init",
    "workspace",
    "config",
    "migrate",
    "update",
    "task",
    "friction",
    "search",
    "run",
    "operation",
    "gc",
    "audit",
    "log",
    "doctor",
    "activity",
    "job",
    "tool",
    "plugin",
    "policy",
    "executor",
    "clock",
    "sweep",
    "routine",
    "auto-task",
    "mcp",
    "web",
    "skill",
    "logs",
    "artifacts",
    "help",
];

fn is_valid_segment(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

/// Whether `name` is an acceptable `metadata.name`: lowercase, digits, `_`,
/// `-`, no dots, and not the reserved `orbit` prefix itself.
pub fn is_valid_namespace(name: &str) -> bool {
    is_valid_segment(name) && name != ORBIT_NAMESPACE_PREFIX
}

/// Whether `verb` is an acceptable `spec.tools[].name`.
pub fn is_valid_verb(verb: &str) -> bool {
    is_valid_segment(verb)
}

/// Canonical registry name of a plugin tool.
pub fn plugin_tool_name(namespace: &str, verb: &str, first_party: bool) -> String {
    if first_party {
        format!("{ORBIT_NAMESPACE_PREFIX}.{namespace}.{verb}")
    } else {
        format!("{namespace}.{verb}")
    }
}

/// Whether a plugin namespace claims a name a built-in tool already owns:
/// the namespace is a built-in tool's leading segment(s), or a built-in tool
/// is exactly one of the plugin's tool names.
pub fn namespace_collides_with_tool(
    namespace: &str,
    first_party: bool,
    builtin_tool: &str,
) -> bool {
    let owned_prefix = if first_party {
        format!("{ORBIT_NAMESPACE_PREFIX}.{namespace}.")
    } else {
        format!("{namespace}.")
    };
    builtin_tool.starts_with(&owned_prefix)
}
