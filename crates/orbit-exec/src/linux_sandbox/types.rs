use super::*;

/// Language-neutral stable workspace mount inside a managed Linux sandbox.
/// Each worker's private namespace binds the real worktree here so toolchain
/// caches that key on absolute paths can hit across worktrees. [ORB-11259]
pub const LINUX_STABLE_WORKSPACE_MOUNT: &str = "/tmp/orbit-workspace";
/// Language-neutral stable build-output mount (`<worktree>/target`) for the
/// same path-normalization seam. Not a shared Cargo target directory.
pub const LINUX_STABLE_BUILD_MOUNT: &str = "/tmp/orbit-build";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapProbeOutcome {
    pub available: bool,
    pub trusted_path: String,
    pub detail: String,
    /// Which trusted binary the probe ran, once one was selected.
    pub source: Option<BwrapSource>,
    /// `bwrap --version` of a binary that passed the capability probe.
    pub version: Option<String>,
}

#[derive(Debug)]
pub struct LinuxBwrapPlan {
    pub wrapper: String,
    pub args: Vec<String>,
    /// Narrow write grants the policy expresses but this argv could not mount,
    /// because their anchor does not exist. Never silently discarded: the
    /// caller reports each one against the rule that granted it.
    pub dropped_grants: Vec<UnsatisfiedWriteGrant>,
    /// Mount-source descriptors retained by the caller until the sandboxed
    /// child exits. Plans use these for writable authority mounts and for
    /// read-only Git metadata hidden by the private `/tmp` mount.
    pub(super) mount_sources: Vec<Arc<File>>,
    pub(super) mount_evidence: Vec<LinuxBwrapMountEvidence>,
    /// Post-run snapshot for the write-policy gaps this plan cannot mount,
    /// taken from the same walk that produced the deny mounts. `None` for a
    /// direct (unmanaged) invocation, which has no post-run guard.
    pub(super) post_run_guard: Option<LinuxBwrapPostRunGuard>,
}

impl PartialEq for LinuxBwrapPlan {
    fn eq(&self, other: &Self) -> bool {
        self.wrapper == other.wrapper
            && self.args == other.args
            && self.dropped_grants == other.dropped_grants
    }
}

impl Eq for LinuxBwrapPlan {}

impl LinuxBwrapPlan {
    /// Descriptor and object identity used by each effective writable `--bind-fd` grant.
    pub fn mount_evidence(&self) -> &[LinuxBwrapMountEvidence] {
        &self.mount_evidence
    }

    /// Hand the compile-time post-run snapshot to the caller that will verify
    /// it after the child exits. The plan itself only needs to outlive spawn.
    pub fn take_post_run_guard(&mut self) -> Option<LinuxBwrapPostRunGuard> {
        self.post_run_guard.take()
    }
}

/// Bounded evidence tying a writable sandbox destination to its open object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapMountEvidence {
    pub destination: PathBuf,
    pub source_fd: i32,
    pub device: u64,
    pub inode: u64,
}

/// Paths a sandboxed child must not read or write: directories, each replaced
/// by one read-only stand-in directory, and single files, each replaced by
/// `/dev/null`.
///
/// The sentinel and every target directory must exist before the plan is
/// compiled: Bubblewrap cannot mount over a path the read-only bind of `/` does
/// not already hold. A masked file that does not exist yet is skipped for the
/// same reason. The stand-ins are bound after all other mounts, and a
/// Bubblewrap mount cannot be undone by a process without capabilities; a
/// nested user namespace receives it locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapMask {
    /// The directory the child sees in place of each target.
    pub sentinel: PathBuf,
    /// The directories hidden from the child.
    pub targets: Vec<PathBuf>,
    /// The single files hidden from the child.
    pub files: Vec<PathBuf>,
}

/// A host object already validated and opened by the runtime owner. Sharing
/// the handle avoids a parent-side `dup`: closing such a duplicate can release
/// unrelated POSIX locks held by SQLite in the same process.
#[derive(Debug)]
pub struct LinuxBwrapMountAuthority {
    pub destination: PathBuf,
    pub source: Arc<File>,
}

#[derive(Debug)]
pub struct LinuxBwrapSpawnRequest<'a> {
    pub plan: &'a LinuxBwrapPlan,
    pub env: &'a [(String, String)],
    pub cwd: Option<&'a Path>,
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
}

/// Snapshot guard for write-policy gaps Bubblewrap cannot represent as a
/// mount at spawn time: a non-subtree deny glob, and an exact or subtree
/// deny whose root does not exist yet. Managed worktrees are disposable and
/// single-writer, so Orbit records existing matches before spawn and rejects
/// any new matches after the child.
///
/// The run's scratch directory (`.orbit/tmp`) is never committed, so a new
/// match there does not fail the run: [`Self::verify`] removes it and reports
/// it instead. A new match anywhere else still fails the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapPostRunGuard {
    profile: String,
    rules: Vec<String>,
    before: BTreeSet<PathBuf>,
}

/// A denyModify match the child created under the run's scratch directory,
/// which [`LinuxBwrapPostRunGuard::verify`] removed instead of failing the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapScratchRemoval {
    /// Canonical path of the removed match.
    pub path: PathBuf,
    /// The denyModify rule body it matched.
    pub rule: String,
}

impl LinuxBwrapPostRunGuard {
    /// Snapshot with a walk of its own. A spawn takes the snapshot from
    /// [`LinuxBwrapPlan::take_post_run_guard`] instead, which reuses the walk
    /// the argv compile already made.
    pub fn capture(profile: &ResolvedFsProfile) -> Result<Option<Self>, OrbitError> {
        let rules = post_run_deny_rules(profile);
        if rules.is_empty() {
            return Ok(None);
        }
        let before = expand_rules(&rules)?;
        Ok(Some(Self {
            profile: profile.name.clone(),
            rules,
            before,
        }))
    }

    /// The snapshot [`Self::capture`] would take, read off a compile's
    /// expansion of the profile's glob rules. An absent exact/subtree deny is
    /// not in that expansion; nothing exists beneath an absent root, so its
    /// contribution to `before` is empty either way.
    pub(super) fn from_expansion(
        profile: &ResolvedFsProfile,
        expanded: &GlobMatches<'_>,
    ) -> Option<Self> {
        let rules = post_run_deny_rules(profile);
        if rules.is_empty() {
            return None;
        }
        let before = rules
            .iter()
            .filter_map(|rule| expanded.get(rule.as_str()))
            .flatten()
            .cloned()
            .collect();
        Some(Self {
            profile: profile.name.clone(),
            rules,
            before,
        })
    }

    /// Name of the fs profile whose deny rules this guard checks.
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Fail on any denyModify match the child created outside `scratch`;
    /// remove the ones beneath it and return them.
    ///
    /// `scratch` is the run's canonical scratch root, resolved before the
    /// child started so the child cannot redirect it. A match anywhere else
    /// fails the run before anything is removed. `None` exempts nothing.
    pub fn verify(
        &self,
        scratch: Option<&Path>,
    ) -> Result<Vec<LinuxBwrapScratchRemoval>, OrbitError> {
        let after = expand_each_rule(self.rules.iter().map(String::as_str))?;
        let mut created = BTreeMap::new();
        for (rule, paths) in after {
            for path in paths.difference(&self.before) {
                created.entry(path.clone()).or_insert(rule);
            }
        }
        let in_scratch =
            |path: &Path| scratch.is_some_and(|root| path != root && path.starts_with(root));
        if let Some(path) = created.keys().find(|path| !in_scratch(path)) {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap child created a path forbidden by denyModify before commit: {}",
                path.display()
            )));
        }
        // Sorted, so a matched directory goes before any match beneath it.
        created
            .into_iter()
            .map(|(path, rule)| {
                remove_scratch_match(&path)?;
                Ok(LinuxBwrapScratchRemoval {
                    path,
                    rule: rule.to_string(),
                })
            })
            .collect()
    }
}

/// Remove one scratch match without following a symlink. A match already
/// removed with a matched ancestor directory is done.
fn remove_scratch_match(path: &Path) -> Result<(), OrbitError> {
    let removed = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap child created a path forbidden by denyModify in run scratch, and removing it failed: {}: {error}",
                path.display()
            )))
        }
        _ => Ok(()),
    }
}
