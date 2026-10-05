//! Compiling a confinement's grant list: the explicit plugin boundary with its
//! read exclusions and requested rights, and the activity profile's read
//! boundary.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::tracing;
use orbit_types::policy::ResolvedFsProfile;

use super::grants::dedupe;
use super::probe::{LandlockProbeOutcome, MINIMUM_LANDLOCK_ABI, RulesetScope};
use super::spawn::child_environment;
use super::{LandlockPathGrant, host, workspace};
use crate::runner::ExecRequest;

/// An explicit confinement: the roots a child may read, the roots it may
/// also modify, and whether it may reach TCP.
///
/// This is the plugin backend's boundary. Unlike the activity path there is
/// no policy profile to compile: the operator granted concrete paths at
/// `orbit plugin enable`, and those are what the ruleset carries beside the
/// host runtime grants every confined child needs. The read restrictions bind
/// both: a host grant never reopens a denied or excluded path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandlockBoundary {
    /// Directories (read as trees) or files the child may read and execute.
    pub read: Vec<PathBuf>,
    /// Directories beneath [`Self::read`] or a host runtime grant (a tool
    /// state directory such as `$GH_CONFIG_DIR`) the child must *not* reach:
    /// the grant is compiled so each of them keeps no granted ancestor, which
    /// refuses listing them as well as reading what is inside. An absent
    /// entry is carved out the same way, so it stays unreachable when it is
    /// created after spawn. A rule binds an
    /// inode and Landlock has no deny, so the ancestors are not granted and
    /// each of their allowed children is granted in its own right — the
    /// ancestors themselves therefore stop being listable. Naming a file in
    /// [`Self::read`] that sits inside a denied directory grants that one
    /// file and nothing else beside it.
    pub read_denies: Vec<PathBuf>,
    /// Absolute glob rules, in the profile grammar, naming paths beneath
    /// [`Self::read`] or a host runtime grant the child must not read: the
    /// read exclusions of the agent run a brokered backend serves (design
    /// `docs/design/plugins/2_agent_call_broker.md` §5). Carved out the way
    /// the activity ruleset carves an agent's own exclusions: the directory
    /// holding an excluded path stays listable, the path itself keeps no
    /// readable ancestor, and a read root at or beneath one is not granted.
    ///
    /// An exact or `<path>/**` rule is carved whether or not its path exists,
    /// so it holds for a name created after spawn. A rule with a wildcard is
    /// carved at the paths it matches when the ruleset is compiled; a name it
    /// matches afterwards is not held, and the spawn reports that rule.
    pub read_exclusions: Vec<String>,
    /// Directories or files the child may also modify. A directory that does
    /// not exist yet is created before spawn: the grant names it, and a rule
    /// cannot bind to an inode that is not there.
    ///
    /// The grant includes read rights, so the child can read what it writes.
    /// Where the root is at or above a [`Self::read_denies`] entry or a
    /// [`Self::read_exclusions`] path, that subtree is carved out of the read
    /// rights: it stays writable, and neither it nor a name created inside it
    /// afterwards is readable. A root with no such overlap is granted whole.
    pub write: Vec<PathBuf>,
    /// Single files the child may modify, named one by one so the grant never
    /// reaches their parent directory. Unlike [`Self::write`], an entry that
    /// is absent — or that is anything other than a regular file — yields no
    /// grant instead of being created: these name files another owner
    /// maintains (a SQLite WAL file set, a lock file), and a symlink standing
    /// where one is expected would otherwise hand the child a writable bind
    /// on whatever it points at.
    ///
    /// A file at or inside a read deny or caller read exclusion is still
    /// writable and is not readable.
    pub write_files: Vec<PathBuf>,
    /// Refuse TCP bind and connect. Requires Landlock ABI 4; an older kernel
    /// fails closed rather than spawning a child with network access.
    pub deny_tcp: bool,
}

/// Device nodes a confined writer opens for output. `/dev/null` is what a
/// shell's `>/dev/null` needs; `/dev/tty` is what an interactive program
/// probes. Neither reaches the filesystem the boundary protects.
const WRITABLE_DEVICES: &[&str] = &["/dev/null", "/dev/tty", "/dev/zero", "/dev/full"];

/// Compile the grant list for
/// [`spawn_under_linux_landlock_boundary`](super::spawn_under_linux_landlock_boundary): the
/// host runtime grants for the child's own environment, the program itself,
/// then the boundary's roots. The read denies and read exclusions are carved
/// out of the host grants as well as the roots.
pub fn linux_landlock_boundary_grants(
    req: &ExecRequest,
    boundary: &LandlockBoundary,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let environment = child_environment(req);
    // A denied tree that does not exist yet is still carved out: its
    // ancestors get no grant, so a directory created there after spawn — a
    // second plugin's first `state/plugins/<ns>` while a long-lived backend
    // runs — has no readable ancestor either.
    let denied: BTreeSet<PathBuf> = boundary
        .read_denies
        .iter()
        .map(|path| crate::path_identity::physical_with_missing_tail(path))
        .collect();
    let excluded = read_exclusion_paths(
        &boundary.read_exclusions,
        &boundary.read,
        &boundary.write,
        &boundary.write_files,
    )?;
    // The host grants follow the child's environment, which can name a
    // denied tree or one above it (`$GH_CONFIG_DIR`, `$ORBIT_ROOT`). Grants
    // are a union, so they are carved here too or they would reopen it.
    let mut grants =
        workspace::carve_out_grants(host::host_read_grants(&environment), &denied, &excluded)?;
    grants.extend(host::program_grants(&req.program, &environment));
    for device in WRITABLE_DEVICES {
        let path = Path::new(device);
        if path.exists() {
            grants.push(LandlockPathGrant::write_file(path.to_path_buf()));
        }
    }
    for root in &boundary.read {
        let Some(path) = existing_canonical(root) else {
            continue;
        };
        if excluded.iter().any(|excluded| path.starts_with(excluded)) {
            continue;
        }
        grants.extend(workspace::carve_out_boundary(&path, &denied, &excluded)?);
    }
    for root in &boundary.write {
        // The same resolution the grant was validated under, materialised
        // without following a link out of it: the rule binds the inode the
        // check decided on, so compiling the boundary cannot widen it
        // [ORB-12799].
        let path = crate::path_identity::create_write_root(root)?;
        warn_if_write_overlaps_read_restriction(&path, &denied, &excluded);
        grants.extend(workspace::carve_out_write(&path, &denied, &excluded)?);
    }
    for file in &boundary.write_files {
        let Some(path) = existing_canonical(file) else {
            continue;
        };
        // `symlink_metadata` on the pre-canonical name, then the canonical
        // path for the rule: an alias inside the boundary is resolved away,
        // and one that stands for a directory or a device is dropped.
        let Ok(metadata) = std::fs::symlink_metadata(file) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        warn_if_write_overlaps_read_restriction(&path, &denied, &excluded);
        grants.extend(workspace::carve_out_write(&path, &denied, &excluded)?);
    }
    Ok(dedupe(grants))
}

/// A write grant includes read. Say so when that read is carved back out
/// because the path meets a read deny or a caller read exclusion.
fn warn_if_write_overlaps_read_restriction(
    path: &Path,
    denied: &BTreeSet<PathBuf>,
    excluded: &BTreeSet<PathBuf>,
) {
    if workspace::write_read_is_carved(path, denied, excluded) {
        tracing::warn!(
            target: "orbit.sandbox.landlock",
            root = %path.display(),
            "landlock write path overlaps a read deny or caller read exclusion; that subtree \
             stays writable but is not granted read",
        );
    }
}

/// The paths [`LandlockBoundary::read_exclusions`] names now: an exact or
/// subtree rule's own path, present or not, and every existing match of a
/// wildcard rule that can reach a read root, a write root, or a write file.
/// A wildcard rule whose literal prefix shares no line of descent with any
/// of those paths carves nothing, so its tree is not walked.
fn read_exclusion_paths(
    rules: &[String],
    read: &[PathBuf],
    write: &[PathBuf],
    write_files: &[PathBuf],
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let roots: Vec<PathBuf> = read
        .iter()
        .chain(write.iter())
        .chain(write_files.iter())
        .map(|root| crate::path_identity::physical_with_missing_tail(root))
        .collect();
    let mut excluded = BTreeSet::new();
    let mut wildcard = Vec::new();
    for rule in rules {
        let body = rule.strip_suffix("/**").unwrap_or(rule);
        if body.contains(['*', '?']) {
            let literal = &rule[..rule.find(['*', '?']).unwrap_or(rule.len())];
            let prefix = Path::new(&literal[..literal.rfind('/').unwrap_or(0).max(1)]);
            if roots
                .iter()
                .any(|root| root.starts_with(prefix) || prefix.starts_with(root))
            {
                wildcard.push(rule.clone());
            }
        } else {
            excluded.insert(crate::path_identity::physical_with_missing_tail(Path::new(
                body,
            )));
        }
    }
    if wildcard.is_empty() {
        return Ok(excluded);
    }
    excluded.extend(crate::linux_sandbox::existing_glob_matches(&wildcard)?);
    tracing::warn!(
        target: "orbit.sandbox.landlock",
        exclusions = wildcard.join(" ").as_str(),
        "landlock holds these read exclusions for the paths they match at spawn; a name they \
         match that is created afterwards is not carved out",
    );
    Ok(excluded)
}

/// Validate requested plugin rights before compiling grants, which can create
/// a missing write root, and before attempting to spawn the child.
pub(super) fn plugin_scope(
    probe: &LandlockProbeOutcome,
    boundary: &LandlockBoundary,
) -> Result<RulesetScope, OrbitError> {
    if !probe.available {
        return Err(OrbitError::PolicyDenied(format!(
            "the plugin sandbox requires Linux Landlock ABI {MINIMUM_LANDLOCK_ABI} or later ({})",
            probe.detail
        )));
    }
    let scope = RulesetScope {
        confine_writes: true,
        deny_tcp: boundary.deny_tcp,
    };
    scope.require_abi(probe.abi)?;
    Ok(scope)
}

/// A read root that is absent grants nothing; the caller's diagnostic names
/// it when a read fails, and creating it would be a write the grant never
/// made.
fn existing_canonical(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// The compiled read boundary for one profile: what the child may read, and
/// which of the profile's exclusions the kernel is not holding for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockReadBoundary {
    /// Every path grant the ruleset will carry.
    pub grants: Vec<LandlockPathGrant>,
    /// Read exclusions this backend cannot enforce for a path that does not
    /// exist yet, as written in the profile.
    ///
    /// Reported rather than dropped so no layer describes the boundary as
    /// whole-contract enforcement. These rules keep their full effect in the
    /// request-time policy check, which decides a concrete path.
    pub unenforced_exclusions: Vec<String>,
}

/// Compile every path grant a confined child needs: the host paths its own
/// runtime requires, and the workspace paths the resolved profile allows.
///
/// `environment` is the child's own environment, which is what names the tool
/// state directories named by the host read set (see `host::host_read_grants`).
/// Nothing outside those grants is readable.
pub fn linux_landlock_read_boundary(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
    environment: &[(String, String)],
) -> Result<LandlockReadBoundary, OrbitError> {
    let workspace_root = workspace_root.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "landlock workspace `{}` must exist and resolve canonically: {error}",
            workspace_root.display()
        ))
    })?;

    let workspace = workspace::workspace_read_grants(&workspace_root, profile)?;
    let mut grants = host::host_read_grants(environment);
    grants.extend(workspace.grants);

    Ok(LandlockReadBoundary {
        grants: dedupe(grants),
        unenforced_exclusions: workspace.unenforced_exclusions,
    })
}
