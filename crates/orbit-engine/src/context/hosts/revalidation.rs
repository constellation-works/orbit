//! What a repaired candidate is revalidated against, and how conflict
//! recovery treats generated files [ORB-16784].

/// `workflow.unlocked_context_paths` beside
/// `workflow.unlocked_path_validation_commands`: a candidate whose tree
/// differs from the last tree its run passed full required validation on
/// only in paths matching `paths` runs `commands` instead of the full list.
/// Empty `commands` always runs the full list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnlockedPathValidation {
    /// Relative path globs (`*` within a component, `**` across them).
    pub paths: Vec<String>,
    /// The commands a candidate that changed only those paths runs.
    pub commands: Vec<String>,
}

/// One `workflow.generated_paths` entry: conflict recovery regenerates a
/// conflicting path matching `path` by running `command`, instead of
/// merging it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedPath {
    /// Relative path glob.
    pub path: String,
    /// Shell command run in the checkout root.
    pub command: String,
}

/// The last tree a run's candidate passed the full required validation
/// list on, as the host that ran it recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTree {
    /// The run whose validation this is.
    pub run_id: String,
    /// The tasks the candidate delivers.
    pub task_ids: Vec<String>,
    /// The checkout the commands ran in.
    pub workspace_path: String,
    /// The validated commit.
    pub head: String,
    /// That commit's tree, which a later candidate's tree is diffed against.
    pub tree: String,
    /// The base the candidate was validated on, when the step named one.
    pub base_sha: Option<String>,
    /// The full command list that passed, deferred-sandbox replays included.
    pub commands: Vec<String>,
}
