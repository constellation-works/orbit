//! Shared friction taxonomy defaults and the friction operation registry.
//!
//! [`operations`] is the single declaration site for every friction verb
//! (ADR-0209 bearing 1, ORB-10358). CLI, MCP, dashboard, and runtime handler
//! wiring all derive from it.

pub mod operations;
pub mod title;

pub use operations::{
    FRICTION_LIST_RESPONSE_MODE_WITH_NOTES, FRICTION_OPERATIONS, FrictionOperation, FrictionVerb,
    friction_operation, friction_operations,
};
pub use title::{FRICTION_TITLE_MAX_CHARS, derive_title, effective_title, normalize_title};

/// Default friction tags and their human-readable glosses.
///
/// This list seeds `.orbit/frictions/tags.yaml` for new workspaces, is merged
/// into existing files when a later default is missing, and keeps tool-schema
/// affordances aligned with the default validator vocabulary.
pub const DEFAULT_FRICTION_TAGS: &[(&str, &str)] = &[
    (
        "automation",
        "Scheduler, auto-task, or delivery-automation friction",
    ),
    ("build", "make/fmt/lint friction"),
    ("docs", "Stale or missing CLAUDE.md or design docs"),
    (
        "history-diverged",
        "A rewritten branch history orphaned recorded automation state",
    ),
    ("lifecycle", "Task lifecycle confusion or transition issues"),
    ("naming", "Naming drift or duplicated sources of truth"),
    ("other", "Fallback"),
    ("policy", "fsProfile or sandboxing surprises"),
    (
        "skill-guidance",
        "Misleading or incorrect skill instructions",
    ),
    ("tooling", "Tool, CLI, or MCP failures"),
];

/// Accepted caller-friendly spellings and the canonical taxonomy categories
/// stored for them.
pub const FRICTION_TAG_ALIASES: &[(&str, &str)] = &[
    ("ci", "build"),
    ("test", "build"),
    ("tests", "build"),
    ("testing", "build"),
    ("cli", "tooling"),
    ("mcp", "tooling"),
    ("tool", "tooling"),
    ("auto-task", "automation"),
    ("auto-tasks", "automation"),
    ("scheduler", "automation"),
    ("routine", "automation"),
    ("skill", "skill-guidance"),
    ("prompt", "skill-guidance"),
];

/// Normalize tag spelling and caller aliases before workspace validation.
///
/// The returned pairs contain only actual alias substitutions, so a tool
/// response can explain why its stored categories differ from the input.
pub fn normalize_friction_tag_aliases(
    raw_tags: Vec<String>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut normalized = Vec::with_capacity(raw_tags.len());
    let mut substitutions = Vec::new();
    for raw in raw_tags {
        let input = raw.trim().to_ascii_lowercase();
        let stored = FRICTION_TAG_ALIASES
            .iter()
            .find_map(|(alias, canonical)| (*alias == input).then_some(*canonical))
            .unwrap_or(input.as_str())
            .to_string();
        if input != stored {
            substitutions.push((input, stored.clone()));
        }
        normalized.push(stored);
    }
    (normalized, substitutions)
}

/// Return the default friction tag enum in schema-friendly literal form.
pub fn friction_tags_literal() -> String {
    DEFAULT_FRICTION_TAGS
        .iter()
        .map(|(tag, _description)| *tag)
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Return the accepted friction-tag aliases in schema-friendly literal form.
pub fn friction_tag_aliases_literal() -> String {
    FRICTION_TAG_ALIASES
        .iter()
        .map(|(alias, canonical)| format!("{alias} → {canonical}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests;
