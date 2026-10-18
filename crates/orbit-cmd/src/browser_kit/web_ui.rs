//! Whether a repository has a web UI that workers would check in a browser,
//! so onboarding knows when to offer the kit.

use std::path::Path;

/// Directories that hold dependencies, build output or Orbit state rather
/// than the repository's own pages.
const SKIPPED_DIRS: [&str; 5] = ["node_modules", "target", "vendor", "dist", "build"];
const MAX_DEPTH: usize = 4;
/// Entries examined before giving up, so a huge tree stays cheap.
const MAX_ENTRIES: usize = 20_000;

/// What marks the repository at `root` as having a web UI: a root
/// `package.json`, or the first HTML file a shallow walk finds (relative
/// path). `None` when neither exists.
pub fn web_ui_marker(root: &Path) -> Option<String> {
    if root.join("package.json").is_file() {
        return Some("package.json".to_string());
    }
    let mut budget = MAX_ENTRIES;
    find_html(root, root, MAX_DEPTH, &mut budget)
}

fn find_html(root: &Path, dir: &Path, depth: usize, budget: &mut usize) -> Option<String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    let mut subdirs = Vec::new();
    for entry in entries {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                subdirs.push(entry.path());
            }
        } else if kind.is_file() && (name.ends_with(".html") || name.ends_with(".htm")) {
            let path = entry.path();
            return Some(
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs
        .iter()
        .find_map(|subdir| find_html(root, subdir, depth - 1, budget))
}
