//! Pre-submission branch validation for public task-pilot callers.

use orbit_common::OrbitError;
use orbit_common::fs::git::{GIT_REMOTE_TIMEOUT, run_git, run_git_within};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Catch a revision passed as a branch before any crew partition is submitted.
    /// Exact origin refs take precedence over local revision resolution, including
    /// when a hex-named branch points somewhere other than the commit it resembles.
    pub fn validate_task_pilot_base_branch(&self, base_branch: &str) -> Result<(), OrbitError> {
        let workspace = &self.paths().repo_root;
        let branch = base_branch.trim();
        let branch = branch.strip_prefix("origin/").unwrap_or(branch).trim();
        let revision = run_git(
            workspace,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "--end-of-options",
                &format!("{branch}^{{commit}}"),
            ],
        )?;
        if !revision.success {
            // Remote-only branches need not resolve in the local object store.
            // Preparation remains responsible for fetching and pinning them.
            return Ok(());
        }
        let branch_ref = format!("refs/heads/{branch}");
        let remotes = run_git(workspace, &["remote"])?;
        let has_origin = remotes
            .stdout
            .lines()
            .any(|remote| remote.trim() == "origin");
        let branch_exists = if has_origin {
            let remote = run_git_within(
                workspace,
                &["ls-remote", "--exit-code", "--heads", "origin", &branch_ref],
                GIT_REMOTE_TIMEOUT,
            )?;
            if !remote.success && remote.code != Some(2) {
                return Err(OrbitError::InvalidInput(format!(
                    "cannot verify task-pilot base branch {branch:?} on origin: {}; check origin and retry",
                    remote.stderr.trim()
                )));
            }
            remote.stdout.lines().any(|line| {
                line.split_once('\t')
                    .is_some_and(|(_, name)| name == branch_ref)
            })
        } else {
            run_git(
                workspace,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{branch_ref}^{{commit}}"),
                ],
            )?
            .success
        };
        if branch_exists {
            return Ok(());
        }
        Err(OrbitError::InvalidInput(format!(
            "task-pilot --base-branch {base_branch:?} resolves to a commit, but no branch with that name exists {}; \
             --base-branch requires a branch name. To pin a source revision, use \
             `orbit run job task_pilot_pipeline --input base_branch=<BRANCH> --input source_revision=<FULL_COMMIT_ID>`; \
             source_revision pins the commit independently of the base branch",
            if has_origin { "on origin" } else { "locally" }
        )))
    }
}
