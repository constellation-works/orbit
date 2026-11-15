//! `[[completion.hold]]`: the rules that keep a completion-authorized
//! (`--complete`) run's task in `review` when its pull request touches a
//! risky path or the task's complexity, type or tags match.
//!
//! Admission refuses a malformed rule at config load, so a typo cannot
//! silently let a risky change land: an unknown field or complexity is a
//! deserialization error, and a bad glob, an empty rule or a blank tag is
//! refused here.

use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{COMPLETION_HOLD_KEY, CompletionHoldRule, normalize_instruction_tag};

/// Admit `completion.hold`: each rule sets at least one of `paths`,
/// `complexity_at_least`, `types` or `tags`; every glob compiles; the
/// complexity is assessed; tags are non-blank. Tags are stored normalized.
/// An error names the rule by its 1-based position. Unset holds nothing.
pub(crate) fn admit_completion_hold(
    raw: Option<Vec<CompletionHoldRule>>,
) -> Result<Vec<CompletionHoldRule>, OrbitError> {
    let mut admitted = Vec::new();
    for (index, mut rule) in raw.unwrap_or_default().into_iter().enumerate() {
        let key = format!("{COMPLETION_HOLD_KEY}[{}]", index + 1);
        let invalid = |reason: String| OrbitError::InvalidInput(format!("{key} {reason}"));
        if rule.is_empty() {
            return Err(invalid(
                "matches nothing; set at least one of `paths`, `complexity_at_least`, `types` \
                 or `tags`"
                    .to_string(),
            ));
        }
        if rule
            .name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err(invalid("has a blank `name`".to_string()));
        }
        if rule.paths.iter().any(|glob| glob.trim().is_empty()) {
            return Err(invalid("has an empty path glob in `paths`".to_string()));
        }
        if !rule.paths.is_empty() && rule.paths.iter().all(|glob| glob.starts_with('!')) {
            return Err(invalid(
                "`paths` only excludes; name at least one glob to match".to_string(),
            ));
        }
        PathFilter::new(&format!("{key}.paths"), &rule.paths)?;
        if rule.complexity_at_least == Some(TaskComplexity::Unassessed) {
            return Err(invalid(
                "`complexity_at_least` must be one of low, medium, hard, xhard".to_string(),
            ));
        }
        let mut tags = Vec::with_capacity(rule.tags.len());
        for tag in &rule.tags {
            let Some(tag) = normalize_instruction_tag(tag) else {
                return Err(invalid("has a blank tag in `tags`".to_string()));
            };
            tags.push(tag);
        }
        rule.tags = tags;
        admitted.push(rule);
    }
    Ok(admitted)
}
