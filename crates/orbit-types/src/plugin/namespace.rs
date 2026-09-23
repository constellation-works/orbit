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
    "plugin",
    "migrate",
    "update",
    "task",
    "friction",
    "search",
    "run",
    "job",
    "tool",
    "gc",
    "audit",
    "log",
    "doctor",
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

// `_` is deliberately not allowed: the MCP surface flattens the canonical
// `<namespace>.<verb>` name to `<namespace>_<verb>` (§4.6), and an
// underscore in either half would let two different plugins produce the
// same flattened name (`a_b` + `c` and `a` + `b_c` both flatten to
// `a_b_c`). `-` has no such ambiguity because it never appears where `.` is
// inserted.
fn is_valid_segment(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `name` is an acceptable `metadata.name`: lowercase, digits, `-`,
/// no dots or underscores, and not the reserved `orbit` prefix itself.
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

/// Whether a plugin namespace claims a built-in tool's leading segment(s):
/// `builtin_tool` starts with the namespace's owned `<ns>.` prefix (or
/// `orbit.<ns>.` for a verified first-party plugin). Checking one of the
/// plugin's own tool names against a built-in name exactly is a separate,
/// per-tool check the caller makes alongside this one.
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
