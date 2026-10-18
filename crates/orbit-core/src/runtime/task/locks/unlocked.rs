//! Declared context that takes no context lock
//! (`workflow.unlocked_context_paths`).

use orbit_common::OrbitError;
use orbit_common::fs::path_glob::RelativePathGlob;
use orbit_common::fs::selector::anchor_path;

/// The admitted `workflow.unlocked_context_paths` globs.
///
/// A matching `context_files` entry stays in the task's context — the agent
/// still receives and reads it — but is left out of the task's lock surface,
/// so two tasks sharing only such a path are admitted together and a text
/// merge reconciles them. A task whose every entry matches (a docs-only task)
/// keeps them all: two rewrites of one document are where a merge can
/// silently produce contradictory text. No globs locks every entry.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnlockedContextPaths {
    globs: Vec<RelativePathGlob>,
}

impl UnlockedContextPaths {
    pub(crate) fn new(patterns: &[String]) -> Result<Self, OrbitError> {
        let globs = patterns
            .iter()
            .map(|pattern| RelativePathGlob::new(pattern))
            .collect::<Result<_, _>>()?;
        Ok(Self { globs })
    }

    /// The selectors of a canonical lock surface that take a lock.
    pub(crate) fn lock_surface(&self, surface: Vec<String>) -> Vec<String> {
        if self.globs.is_empty() || surface.iter().all(|selector| self.unlocks(selector)) {
            return surface;
        }
        surface
            .into_iter()
            .filter(|selector| !self.unlocks(selector))
            .collect()
    }

    /// Whether a selector's filesystem anchor matches a glob. A selector
    /// with no anchor (`module:`, `command:`) always locks.
    fn unlocks(&self, selector: &str) -> bool {
        anchor_path(selector).ok().is_some_and(|path| {
            path.to_str()
                .is_some_and(|path| self.globs.iter().any(|glob| glob.matches(path)))
        })
    }
}
