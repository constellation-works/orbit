//! What an implementation run changed after a `reject`: the paths whose
//! content differs between the rejected candidate and the next reviewed one.
//! Read from the workspace repository without fetching; a commit the
//! repository no longer has leaves the reject unjudged.

use std::collections::BTreeSet;
use std::path::Path;

use crate::OrbitRuntime;

/// The shortest commit name a settlement may abbreviate to.
const MIN_COMMIT_CHARS: usize = 7;

/// The workspace repository, opened once per scan.
pub(super) struct Candidates {
    repo: Option<git2::Repository>,
}

impl Candidates {
    pub(super) fn open(runtime: &OrbitRuntime) -> Self {
        Self {
            repo: git2::Repository::open(&runtime.paths().repo_root).ok(),
        }
    }

    /// The `paths` whose content differs between commits `from` and `to`; a
    /// path present in only one of them counts as changed. A path given as
    /// `path:line` is read as `path`. `None` when either commit is not a
    /// commit name, the repository cannot read it, or a cited path resolves
    /// in neither tree.
    pub(super) fn changed(
        &self,
        from: &str,
        to: &str,
        paths: &BTreeSet<String>,
    ) -> Option<BTreeSet<String>> {
        let repo = self.repo.as_ref()?;
        let tree = |name: &str| {
            let name = name.trim();
            if name.len() < MIN_COMMIT_CHARS || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            repo.revparse_single(name).ok()?.peel_to_tree().ok()
        };
        let (before, after) = (tree(from)?, tree(to)?);
        let entry = |tree: &git2::Tree<'_>, path: &str| {
            tree.get_path(Path::new(without_line(path)))
                .ok()
                .map(|entry| entry.id())
        };
        let mut changed = BTreeSet::new();
        for path in paths {
            let before_entry = entry(&before, path);
            let after_entry = entry(&after, path);
            match (before_entry, after_entry) {
                (None, None) => return None,
                (before_entry, after_entry) if before_entry != after_entry => {
                    changed.insert(path.clone());
                }
                _ => {}
            }
        }
        Some(changed)
    }
}

/// `path` without a trailing `:line`.
fn without_line(path: &str) -> &str {
    match path.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) => file,
        _ => path,
    }
}
