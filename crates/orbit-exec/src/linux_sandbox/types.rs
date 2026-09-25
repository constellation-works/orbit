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
    /// child exits. Empty for ordinary path-based plans and audit rendering.
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
    /// Descriptor and object identity used by each effective `--bind-fd` grant.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapPostRunGuard {
    rules: Vec<String>,
    before: BTreeSet<PathBuf>,
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
        Ok(Some(Self { rules, before }))
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
        Some(Self { rules, before })
    }

    pub fn verify(&self) -> Result<(), OrbitError> {
        let after = expand_rules(&self.rules)?;
        let created = after.difference(&self.before).next();
        if let Some(path) = created {
            return Err(OrbitError::PolicyDenied(format!(
                "linux-bwrap child created a path forbidden by denyModify before commit: {}",
                path.display()
            )));
        }
        Ok(())
    }
}
