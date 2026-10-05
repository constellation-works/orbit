//! The `epic` size tag and the one admission rule that still reads hierarchy.
//!
//! Epic execution is retired: a tagged root no longer owns a pipeline, and it
//! no longer reserves the union of its descendants' `context_files`. Every task
//! reserves exactly what it declares.
//!
//! That leaves one population unsafe to admit. A root that declared nothing of
//! its own, while a descendant declared context, was relying on the union — so
//! it would now be dispatched holding no reservation at all, beside the very
//! children whose files that union covered. [`inherited_only_epic_roots`] is
//! the single definition of that population, applied by the migration report,
//! by backlog admission, and by task-lock reservation. It lives here, in the
//! lowest layer, so those three cannot each decide what a "descendant" is.

use std::collections::{BTreeMap, BTreeSet};

use crate::task::model::Task;

/// The size hint: one large task a top-tier crew takes on whole. It informs
/// crew selection; it does not order, group, or widen admission.
pub const EPIC_TAG: &str = "epic";

/// Guard on a parent walk. A malformed store must cost a bounded walk, not a
/// hang.
const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

/// One task as the epic rule reads it: identity, parentage, tags, and whether
/// it declared any context at all.
///
/// Deliberately a borrowed view rather than a task type, so a caller holding
/// full tasks and a caller holding envelopes answer the question the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpicHierarchyNode<'a> {
    pub id: &'a str,
    pub parent_id: Option<&'a str>,
    pub tags: &'a [String],
    /// Whether the task declared any `context_files` entries. A selector for a
    /// file that does not exist yet is a declaration like any other: what
    /// matters here is that the task named its own surface.
    pub declares_context: bool,
}

impl<'a> From<&'a Task> for EpicHierarchyNode<'a> {
    fn from(task: &'a Task) -> Self {
        Self {
            id: task.id.as_str(),
            parent_id: task.parent_id(),
            tags: &task.tags,
            declares_context: !task.context_files.is_empty(),
        }
    }
}

/// Whether `tags` carries the [`EPIC_TAG`] size hint.
pub fn has_epic_tag(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == EPIC_TAG)
}

/// The `epic`-tagged tasks that declared no context of their own while a
/// descendant declared some, each mapped to those descendants in ID order.
///
/// A root reaches this map only when something was actually inherited from
/// below: an `epic`-tagged task whose whole family declares nothing has no
/// footprint to lose and is an ordinary undeclared task. A descendant is
/// attributed to its *nearest* `epic`-tagged ancestor, so a nested root owns
/// its own subtree rather than the outer one owning it twice.
///
/// Every caller uses this as both the rule and the diagnostic: the keys are
/// what admission withholds and reservation refuses, and the values are the
/// material an operator writes the root's own `context_files` from.
pub fn inherited_only_epic_roots<'a>(
    nodes: impl IntoIterator<Item = EpicHierarchyNode<'a>>,
) -> BTreeMap<&'a str, Vec<&'a str>> {
    let by_id: BTreeMap<&str, EpicHierarchyNode<'a>> =
        nodes.into_iter().map(|node| (node.id, node)).collect();
    let mut roots: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in by_id.values() {
        if !node.declares_context {
            continue;
        }
        let Some(root_id) = nearest_epic_ancestor(node, &by_id) else {
            continue;
        };
        if by_id
            .get(root_id)
            .is_some_and(|root| !root.declares_context)
        {
            roots.entry(root_id).or_default().push(node.id);
        }
    }
    for descendants in roots.values_mut() {
        descendants.sort_unstable();
    }
    roots
}

/// The nearest `epic`-tagged ancestor of `node`. Cycle- and depth-guarded.
fn nearest_epic_ancestor<'a>(
    node: &EpicHierarchyNode<'a>,
    by_id: &BTreeMap<&'a str, EpicHierarchyNode<'a>>,
) -> Option<&'a str> {
    let mut visited = BTreeSet::from([node.id]);
    let mut next_parent_id = node.parent_id;
    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let parent_id = next_parent_id?;
        if !visited.insert(parent_id) {
            return None;
        }
        let parent = by_id.get(parent_id)?;
        if has_epic_tag(parent.tags) {
            return Some(parent.id);
        }
        next_parent_id = parent.parent_id;
    }
    None
}
