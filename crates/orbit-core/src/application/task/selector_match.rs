/// Test whether any of a task's `context_files` selectors apply to `query_path`.
///
/// Selectors take three forms: `file:<path>`, `dir:<path>`, and
/// `symbol:<file>#<name>:<kind>`. A bare path (no prefix) is treated as a
/// file selector. Matching is bidirectional path-containment:
///
/// - exact equality matches.
/// - `query_path` lies within a scope directory.
/// - `scope` lies within a query directory (when the user passes a parent
///   directory, every selector under it matches).
///
/// All three selector forms collapse to a single normalized scope path
/// before the comparison.
pub fn task_selectors_contain_path(selectors: &[String], query_path: &str) -> bool {
    let query = normalize_path_for_match(query_path);
    selectors
        .iter()
        .any(|selector| selector_matches_path(selector, &query))
}

fn selector_matches_path(selector: &str, query: &str) -> bool {
    let scope = if let Some(after) = selector.strip_prefix("file:") {
        after
    } else if let Some(after) = selector.strip_prefix("dir:") {
        after
    } else if let Some(after) = selector.strip_prefix("symbol:") {
        // symbol:<file>#<name>:<kind> — keep only the file portion.
        after.split('#').next().unwrap_or(after)
    } else {
        selector
    };
    let scope = normalize_path_for_match(scope);
    paths_overlap(&scope, query)
}

fn normalize_path_for_match(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace('\\', "/")
}

fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return !a.is_empty();
    }
    is_within(a, b) || is_within(b, a)
}

fn is_within(inner: &str, outer: &str) -> bool {
    if outer.is_empty() {
        return false;
    }
    if let Some(rest) = inner.strip_prefix(outer) {
        return rest.starts_with('/');
    }
    false
}
