//! Linux Bubblewrap write-confinement for CLI-backed agents.
//!
//! The backend deliberately keeps the host filesystem readable and materializes
//! `ResolvedFsProfile::modify` as ordered bind mounts. It is therefore honest
//! write confinement, not a general read-policy implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use orbit_common::OrbitError;
use orbit_common::fs::glob::compile_glob_regex;
use orbit_types::policy::ResolvedFsProfile;

const TRUSTED_BWRAP_PATH: &str = "/usr/bin/bwrap";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxBwrapPlan {
    pub wrapper: String,
    pub args: Vec<String>,
    /// Narrow write grants the policy expresses but this argv could not mount,
    /// because their anchor does not exist. Never silently discarded: the
    /// caller reports each one against the rule that granted it.
    pub dropped_grants: Vec<UnsatisfiedWriteGrant>,
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

/// Snapshot guard for Bubblewrap's one known write-policy gap: a
/// non-subtree deny glob cannot be represented for a path that does not exist
/// yet. Managed worktrees are disposable and single-writer, so Orbit records
/// existing matches before spawn and rejects any new matches after the child.
#[derive(Debug, Clone)]
pub struct LinuxBwrapPostRunGuard {
    rules: Vec<String>,
    before: BTreeSet<PathBuf>,
}

impl LinuxBwrapPostRunGuard {
    pub fn capture(profile: &ResolvedFsProfile) -> Result<Option<Self>, OrbitError> {
        let rules = non_subtree_denies(profile);
        if rules.is_empty() {
            return Ok(None);
        }
        let before = expand_rules(&rules)?;
        Ok(Some(Self { rules, before }))
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

/// The filesystem shape a granted write anchor must have for Bubblewrap to
/// bind it. Derived from the granting rule, never from a table of known paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteAnchorKind {
    File,
    Directory,
}

/// One narrow write exception the effective profile grants, together with the
/// mount anchor Bubblewrap needs on disk in order to honor it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteGrant {
    /// The `modify` rule, exactly as the effective profile spells it.
    pub rule: String,
    /// The path that must exist for the rule to become a mount.
    pub anchor: PathBuf,
    pub kind: WriteAnchorKind,
}

/// A grant that policy expresses but the mount plan cannot honor. Carries the
/// path *and* the rule so a denial is attributable without reading argv.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnsatisfiedWriteGrant {
    pub rule: String,
    pub anchor: PathBuf,
    pub reason: String,
}

impl UnsatisfiedWriteGrant {
    pub fn describe(&self) -> String {
        format!(
            "write grant `{}` (rule `{}`) was not applied: {}",
            self.anchor.display(),
            self.rule,
            self.reason
        )
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreparedWriteGrants {
    /// Anchors materialized by this call, in profile order.
    pub created: Vec<PathBuf>,
    /// Grants left unmountable because their anchor is absent and lies outside
    /// the trusted preparation root, so creating it is not this layer's call.
    pub unsatisfied: Vec<UnsatisfiedWriteGrant>,
}

/// Every narrow write re-allow in the effective profile — the positive
/// exact/subtree rules nested under an earlier deny — paired with the anchor
/// each one needs. Broad writable roots are excluded: they are required to
/// exist by [`compile_linux_bwrap_argv`] and are not exceptions to a deny.
///
/// This is the whole grant set. It is read off the profile that will compile
/// the argv, so it cannot drift from what the sandbox actually enforces.
pub fn linux_bwrap_write_grants(
    profile: &ResolvedFsProfile,
) -> Result<Vec<WriteGrant>, OrbitError> {
    let mut grants = Vec::new();
    for (index, rule) in profile.modify.iter().enumerate() {
        if rule.starts_with('!') || !is_narrow_reallow(&profile.modify[..index], rule) {
            continue;
        }
        let Some(anchor) = exact_or_subtree_root(rule) else {
            continue;
        };
        // A later deny that covers the anchor shadows this re-allow under the
        // profile's last-match-wins contract. Do not materialize a path the
        // final policy denies. A narrower deny below a subtree does not match
        // the subtree root, so the remaining writable portion is preserved.
        if !path_is_effectively_writable(profile, &anchor)? {
            continue;
        }
        grants.push(WriteGrant {
            rule: rule.clone(),
            anchor,
            kind: write_anchor_kind(rule),
        });
    }
    Ok(grants)
}

/// Materialize every granted-but-absent write anchor that falls inside a
/// trusted, disposable preparation root (the managed worktree).
///
/// A positive re-allow only becomes a bind mount if its anchor exists, so an
/// absent anchor silently leaves the path under the surrounding read-only
/// bind. Creating the anchor grants nothing the policy did not already grant —
/// the effective profile is the sole authority for *which* paths appear here.
pub fn prepare_linux_bwrap_write_grants(
    profile: &ResolvedFsProfile,
    containment_root: &Path,
) -> Result<PreparedWriteGrants, OrbitError> {
    let root = containment_root.canonicalize().map_err(|error| {
        OrbitError::Execution(format!(
            "canonicalize trusted write-grant preparation root `{}`: {error}",
            containment_root.display()
        ))
    })?;
    let mut prepared = PreparedWriteGrants::default();
    for grant in linux_bwrap_write_grants(profile)? {
        if ensure_write_anchor(&root, &grant, &mut prepared)? {
            prepared.created.push(grant.anchor);
        }
    }
    Ok(prepared)
}

/// Explain a path against the effective profile's write rules: `None` when the
/// path is granted, otherwise the deny that shadows it and the exception that
/// would be needed. This is what turns an EROFS into an attributable refusal.
pub fn linux_bwrap_write_grant_diagnostic(
    profile: &ResolvedFsProfile,
    path: &Path,
) -> Result<Option<String>, OrbitError> {
    let rendered = path.to_string_lossy().replace('\\', "/");
    let mut decision: Option<&str> = None;
    for rule in &profile.modify {
        let body = rule.strip_prefix('!').unwrap_or(rule.as_str());
        if compile_glob_regex(body)
            .map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "invalid linux-bwrap filesystem glob `{body}`: {error}"
                ))
            })?
            .is_match(&rendered)
        {
            decision = Some(rule);
        }
    }
    match decision {
        Some(rule) if !rule.starts_with('!') => Ok(None),
        Some(denied) => Ok(Some(format!(
            "`{}` is not writable inside the sandbox: denyModify rule `{}` shadows it and no later narrow re-allow grants it; add an exception such as `{}` to the effective policy",
            path.display(),
            denied,
            rendered
        ))),
        None => Ok(Some(format!(
            "`{}` is not writable inside the sandbox: no modify rule in fsProfile `{}` grants it",
            path.display(),
            profile.name
        ))),
    }
}

/// Returns `Ok(true)` when this call created the anchor. Records an
/// unsatisfied grant instead of creating anything outside `root`.
fn ensure_write_anchor(
    root: &Path,
    grant: &WriteGrant,
    prepared: &mut PreparedWriteGrants,
) -> Result<bool, OrbitError> {
    let Ok(relative) = grant.anchor.strip_prefix(root) else {
        return inspect_host_owned_anchor(root, grant, prepared);
    };

    // Validate the whole worktree-owned chain before consulting the final
    // target. `symlink_metadata(anchor)` follows intermediate symlinks, so an
    // existing outside target would otherwise bypass the absent-anchor checks.
    validate_owned_anchor_components(root, relative, grant)?;

    match std::fs::symlink_metadata(&grant.anchor) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must not be a symlink",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            let canonical = grant.anchor.canonicalize().map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must resolve canonically inside `{}`: {error}",
                    grant.anchor.display(),
                    grant.rule,
                    root.display()
                ))
            })?;
            if !canonical.starts_with(root) {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves outside the trusted preparation root `{}` as `{}`",
                    grant.anchor.display(),
                    grant.rule,
                    root.display(),
                    canonical.display()
                )));
            }
            let matches = match grant.kind {
                WriteAnchorKind::File => metadata.is_file(),
                WriteAnchorKind::Directory => metadata.is_dir(),
            };
            if !matches {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) exists with the wrong filesystem type",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            return Ok(false);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(OrbitError::Execution(format!(
                "inspect write-grant anchor `{}` (rule `{}`): {error}",
                grant.anchor.display(),
                grant.rule
            )));
        }
    }

    if let Some(parent) = grant.anchor.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrbitError::Execution(format!(
                "create write-grant anchor parent `{}` (rule `{}`): {error}",
                parent.display(),
                grant.rule
            ))
        })?;
        let canonical_parent = parent.canonicalize().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "write-grant anchor parent `{}` (rule `{}`) must resolve canonically inside `{}`: {error}",
                parent.display(),
                grant.rule,
                root.display()
            ))
        })?;
        if !canonical_parent.starts_with(root) {
            return Err(OrbitError::InvalidInput(format!(
                "write-grant anchor parent `{}` (rule `{}`) resolves outside the trusted preparation root `{}` as `{}`",
                parent.display(),
                grant.rule,
                root.display(),
                canonical_parent.display()
            )));
        }
    }
    match grant.kind {
        WriteAnchorKind::File => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&grant.anchor)
                .map_err(|error| {
                    OrbitError::Execution(format!(
                        "create write-grant anchor file `{}` (rule `{}`): {error}",
                        grant.anchor.display(),
                        grant.rule
                    ))
                })?;
        }
        WriteAnchorKind::Directory => {
            std::fs::create_dir(&grant.anchor).map_err(|error| {
                OrbitError::Execution(format!(
                    "create write-grant anchor directory `{}` (rule `{}`): {error}",
                    grant.anchor.display(),
                    grant.rule
                ))
            })?;
        }
    }
    Ok(true)
}

fn inspect_host_owned_anchor(
    root: &Path,
    grant: &WriteGrant,
    prepared: &mut PreparedWriteGrants,
) -> Result<bool, OrbitError> {
    match std::fs::symlink_metadata(&grant.anchor) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) must not be a symlink",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            let matches = match grant.kind {
                WriteAnchorKind::File => metadata.is_file(),
                WriteAnchorKind::Directory => metadata.is_dir(),
            };
            if !matches {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) exists with the wrong filesystem type",
                    grant.anchor.display(),
                    grant.rule
                )));
            }
            Ok(false)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            prepared.unsatisfied.push(UnsatisfiedWriteGrant {
                rule: grant.rule.clone(),
                anchor: grant.anchor.clone(),
                reason: format!(
                    "the anchor does not exist and lies outside the trusted preparation root `{}`, so the host must create it before dispatch",
                    root.display()
                ),
            });
            Ok(false)
        }
        Err(error) => Err(OrbitError::Execution(format!(
            "inspect write-grant anchor `{}` (rule `{}`): {error}",
            grant.anchor.display(),
            grant.rule
        ))),
    }
}

fn validate_owned_anchor_components(
    root: &Path,
    relative: &Path,
    grant: &WriteGrant,
) -> Result<(), OrbitError> {
    let mut current = root.to_path_buf();
    let component_count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let std::path::Component::Normal(component) = component else {
            return Err(OrbitError::InvalidInput(format!(
                "write-grant anchor `{}` (rule `{}`) must not contain non-normal path components inside `{}`",
                grant.anchor.display(),
                grant.rule,
                root.display()
            )));
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves through symlink `{}`; components inside the preparation root must not be a symlink",
                    grant.anchor.display(),
                    grant.rule,
                    current.display()
                )));
            }
            Ok(metadata) if index + 1 < component_count && !metadata.is_dir() => {
                return Err(OrbitError::InvalidInput(format!(
                    "write-grant anchor `{}` (rule `{}`) resolves through non-directory component `{}`",
                    grant.anchor.display(),
                    grant.rule,
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(OrbitError::Execution(format!(
                    "inspect write-grant anchor component `{}` (rule `{}`): {error}",
                    current.display(),
                    grant.rule
                )));
            }
        }
    }
    Ok(())
}

/// The policy grammar is the anchor-type contract: an exact rule denotes one
/// file, while `<root>/**` denotes a directory subtree. Filename punctuation
/// is never evidence, so extensionless files and dotted directories are both
/// represented without a hardcoded path inventory.
fn write_anchor_kind(rule: &str) -> WriteAnchorKind {
    if rule.ends_with("/**") {
        WriteAnchorKind::Directory
    } else {
        WriteAnchorKind::File
    }
}

fn path_is_effectively_writable(
    profile: &ResolvedFsProfile,
    path: &Path,
) -> Result<bool, OrbitError> {
    let rendered = path.to_string_lossy().replace('\\', "/");
    let mut writable = false;
    for rule in &profile.modify {
        let body = rule.strip_prefix('!').unwrap_or(rule.as_str());
        if compile_glob_regex(body)
            .map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "invalid linux-bwrap filesystem glob `{body}`: {error}"
                ))
            })?
            .is_match(&rendered)
        {
            writable = !rule.starts_with('!');
        }
    }
    Ok(writable)
}

pub fn bwrap_program_for_audit() -> &'static str {
    TRUSTED_BWRAP_PATH
}

pub fn bwrap_path() -> Option<PathBuf> {
    let path = Path::new(TRUSTED_BWRAP_PATH);
    (cfg!(target_os = "linux") && is_executable(path)).then(|| path.to_path_buf())
}

pub fn bwrap_unavailable_message() -> String {
    format!("trusted Bubblewrap not available at {TRUSTED_BWRAP_PATH}")
}

/// Probe the namespaces and mounts Orbit relies on rather than treating a
/// present binary as usable. The host network namespace is retained
/// explicitly with `--share-net`.
pub fn probe_bwrap() -> BwrapProbeOutcome {
    let Some(path) = bwrap_path() else {
        return BwrapProbeOutcome {
            available: false,
            trusted_path: TRUSTED_BWRAP_PATH.to_string(),
            detail: bwrap_unavailable_message(),
        };
    };
    let args = base_namespace_args();
    let output = Command::new(&path)
        .args(&args)
        .arg("--")
        .arg("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() => BwrapProbeOutcome {
            available: true,
            trusted_path: path.display().to_string(),
            detail: "capability probe succeeded".to_string(),
        },
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            BwrapProbeOutcome {
                available: false,
                trusted_path: path.display().to_string(),
                detail: format!(
                    "Bubblewrap capability probe failed{}{}",
                    if detail.is_empty() { "" } else { ": " },
                    detail
                ),
            }
        }
        Err(error) => BwrapProbeOutcome {
            available: false,
            trusted_path: path.display().to_string(),
            detail: format!("Bubblewrap capability probe could not execute: {error}"),
        },
    }
}

/// Compile a deterministic Bubblewrap argv. Broad writable roots are emitted
/// before every deny mount. A positive exact/subtree rule that is strictly
/// nested under an earlier deny is emitted in rule order as an explicit narrow
/// re-allow; positive ancestors and equal roots cannot override a deny.
///
/// A narrow re-allow whose anchor is absent cannot be mounted. It is reported
/// on [`LinuxBwrapPlan::dropped_grants`] rather than dropped, so the caller can
/// attribute the resulting denial to a path and a rule instead of leaving the
/// sandboxed process to interpret an EROFS.
pub fn compile_linux_bwrap_argv(
    profile: &ResolvedFsProfile,
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    managed_worktree: bool,
) -> Result<LinuxBwrapPlan, OrbitError> {
    let mut out = base_namespace_args();
    let mut dropped_grants = Vec::new();
    // Replace mutable host pseudo-filesystems and scratch before applying
    // policy mounts. A writable worktree nested below `/tmp` is then re-bound
    // narrowly without exposing the rest of the host scratch tree.
    out.extend([
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ]);
    let writable_roots = positive_mount_roots(profile)?;

    for (index, rule) in profile.modify.iter().enumerate() {
        if rule.starts_with('!') || is_narrow_reallow(&profile.modify[..index], rule) {
            continue;
        }
        for path in mount_paths_for_rule(rule, true)? {
            push_mount(&mut out, "--bind", &path);
        }
    }
    for (index, rule) in profile.modify.iter().enumerate() {
        if let Some(denied) = rule.strip_prefix('!') {
            if !is_exact_or_subtree(denied)
                && !managed_worktree
                && overlaps_writable_root(denied, &writable_roots)
            {
                return Err(OrbitError::PolicyDenied(format!(
                    "linux-bwrap cannot enforce non-subtree denyModify `{denied}` for a direct invocation; use a managed worktree"
                )));
            }
            for path in mount_paths_for_rule(denied, false)? {
                push_mount(&mut out, "--ro-bind", &path);
            }
        } else if is_narrow_reallow(&profile.modify[..index], rule) {
            let Some(anchor) = exact_or_subtree_root(rule) else {
                continue;
            };
            if !path_is_effectively_writable(profile, &anchor)? {
                continue;
            }
            let paths = mount_paths_for_rule(rule, false)?;
            if paths.is_empty() {
                dropped_grants.push(UnsatisfiedWriteGrant {
                    rule: rule.clone(),
                    anchor,
                    reason: "no path on disk matches the rule, so Bubblewrap has nothing to bind and the grant stays under the surrounding read-only mount".to_string(),
                });
                continue;
            }
            for path in paths {
                push_mount(&mut out, "--bind", &path);
            }
        }
    }

    if let Some(cwd) = cwd {
        let cwd = canonical_existing(cwd, "sandbox cwd")?;
        if managed_worktree && cwd_is_writable_root(&cwd, &writable_roots) {
            append_stable_toolchain_mounts(&mut out, &cwd)?;
        }
        out.push("--chdir".to_string());
        out.push(cwd.display().to_string());
    }
    out.push("--".to_string());
    out.push(program.to_string());
    out.extend(args.iter().cloned());

    Ok(LinuxBwrapPlan {
        wrapper: TRUSTED_BWRAP_PATH.to_string(),
        args: out,
        dropped_grants,
    })
}

pub fn spawn_under_linux_bwrap(request: LinuxBwrapSpawnRequest<'_>) -> Result<Child, OrbitError> {
    let LinuxBwrapSpawnRequest {
        plan,
        env,
        cwd,
        stdin,
        stdout,
        stderr,
    } = request;
    if plan.wrapper != TRUSTED_BWRAP_PATH {
        return Err(OrbitError::Execution(format!(
            "refusing untrusted Bubblewrap wrapper `{}`",
            plan.wrapper
        )));
    }
    let mut command = Command::new(TRUSTED_BWRAP_PATH);
    command
        // `env` is the complete child environment the caller composed from the
        // `[execution.env]` allowlist; the sandbox adds nothing ambient of its
        // own. Bubblewrap passes its own environment through to the confined
        // program, so anything seeded here reaches the provider. [ORB-10917]
        .args(&plan.args)
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map_err(|error| {
        OrbitError::Execution(format!(
            "failed to spawn trusted Bubblewrap `{TRUSTED_BWRAP_PATH}`: {error}"
        ))
    })
}

fn base_namespace_args() -> Vec<String> {
    [
        "--die-with-parent",
        "--new-session",
        "--unshare-all",
        "--share-net",
        "--ro-bind",
        "/",
        "/",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn cwd_is_writable_root(cwd: &Path, writable_roots: &[PathBuf]) -> bool {
    writable_roots
        .iter()
        .any(|root| cwd.starts_with(root) || root.starts_with(cwd))
}

/// Bind the managed worktree (and its `target/` directory) at stable `/tmp`
/// paths inside this sandbox's private tmpfs. Toolchain wrappers can rewrite
/// absolute paths onto those mounts so compiler caches hit across worktrees
/// without sharing a mutable Cargo target directory. [ORB-11259]
fn append_stable_toolchain_mounts(out: &mut Vec<String>, cwd: &Path) -> Result<(), OrbitError> {
    let target = cwd.join("target");
    std::fs::create_dir_all(&target).map_err(|error| {
        OrbitError::Execution(format!(
            "create managed-worktree target dir `{}` for stable build mount: {error}",
            target.display()
        ))
    })?;
    let target = canonical_existing(&target, "stable build mount")?;
    out.extend([
        "--dir".to_string(),
        LINUX_STABLE_WORKSPACE_MOUNT.to_string(),
        "--bind".to_string(),
        cwd.display().to_string(),
        LINUX_STABLE_WORKSPACE_MOUNT.to_string(),
        "--dir".to_string(),
        LINUX_STABLE_BUILD_MOUNT.to_string(),
        "--bind".to_string(),
        target.display().to_string(),
        LINUX_STABLE_BUILD_MOUNT.to_string(),
    ]);
    Ok(())
}

fn push_mount(args: &mut Vec<String>, option: &str, path: &Path) {
    let rendered = path.display().to_string();
    args.push(option.to_string());
    args.push(rendered.clone());
    args.push(rendered);
}

fn positive_mount_roots(profile: &ResolvedFsProfile) -> Result<Vec<PathBuf>, OrbitError> {
    let mut roots = BTreeSet::new();
    for rule in profile.modify.iter().filter(|rule| !rule.starts_with('!')) {
        for root in mount_paths_for_rule(rule, false)? {
            roots.insert(root);
        }
    }
    Ok(roots.into_iter().collect())
}

fn mount_paths_for_rule(rule: &str, require_match: bool) -> Result<Vec<PathBuf>, OrbitError> {
    if is_exact_or_subtree(rule) {
        let root = rule.strip_suffix("/**").unwrap_or(rule);
        if !Path::new(root).exists() && !require_match {
            return Ok(Vec::new());
        }
        let path = canonical_existing(Path::new(root), "sandbox mount")?;
        return Ok(vec![path]);
    }
    let matches = expand_rule(rule)?;
    if require_match && matches.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "linux-bwrap modify rule `{rule}` has no existing path to mount"
        )));
    }
    Ok(matches.into_iter().collect())
}

fn is_narrow_reallow(prior_rules: &[String], rule: &str) -> bool {
    let Some(root) = exact_or_subtree_root(rule) else {
        return false;
    };
    prior_rules
        .iter()
        .filter_map(|prior| prior.strip_prefix('!'))
        .filter_map(exact_or_subtree_root)
        .any(|denied| root != denied && root.starts_with(&denied))
}

fn exact_or_subtree_root(rule: &str) -> Option<PathBuf> {
    is_exact_or_subtree(rule).then(|| PathBuf::from(rule.strip_suffix("/**").unwrap_or(rule)))
}

fn is_exact_or_subtree(rule: &str) -> bool {
    let body = rule.strip_suffix("/**").unwrap_or(rule);
    !body.contains(['*', '?'])
}

fn overlaps_writable_root(rule: &str, roots: &[PathBuf]) -> bool {
    let prefix = static_prefix(rule);
    roots
        .iter()
        .any(|root| root.starts_with(&prefix) || prefix.starts_with(root))
}

fn non_subtree_denies(profile: &ResolvedFsProfile) -> Vec<String> {
    profile
        .modify
        .iter()
        .filter_map(|rule| rule.strip_prefix('!'))
        .filter(|rule| !is_exact_or_subtree(rule))
        .map(str::to_string)
        .collect()
}

/// Every existing path matched by any of `rules`.
///
/// Rules are grouped by the directory their static prefix resolves to and
/// each such directory is walked once. The shipped default policy carries
/// four non-subtree denies (`**/.env` and friends) whose prefix is the
/// workspace root, and the post-run guard expands them before and after every
/// sandboxed invocation; one walk per rule per phase made that eight full
/// workspace traversals (including `target/` and `.git/`) per agent step.
fn expand_rules(rules: &[String]) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let mut by_root: BTreeMap<PathBuf, Vec<_>> = BTreeMap::new();
    for rule in rules {
        let regex = compile_glob_regex(rule).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid linux-bwrap filesystem glob `{rule}`: {error}"
            ))
        })?;
        let root = nearest_existing_ancestor(&static_prefix(rule))?;
        by_root.entry(root).or_default().push(regex);
    }
    let mut matches = BTreeSet::new();
    for (root, regexes) in by_root {
        let mut candidates = Vec::new();
        walk_paths(&root, &mut candidates)?;
        for candidate in candidates {
            let rendered = candidate.to_string_lossy().replace('\\', "/");
            if regexes.iter().any(|regex| regex.is_match(&rendered)) {
                matches.insert(canonical_existing(&candidate, "denyModify match")?);
            }
        }
    }
    Ok(matches)
}

fn expand_rule(rule: &str) -> Result<BTreeSet<PathBuf>, OrbitError> {
    expand_rules(std::slice::from_ref(&rule.to_string()))
}

fn static_prefix(rule: &str) -> PathBuf {
    let wildcard = rule.find(['*', '?']).unwrap_or(rule.len());
    let literal = &rule[..wildcard];
    let boundary = literal.rfind('/').unwrap_or(0);
    let prefix = if wildcard == rule.len() {
        literal
    } else if boundary == 0 {
        "/"
    } else {
        &literal[..boundary]
    };
    PathBuf::from(prefix)
}

fn nearest_existing_ancestor(path: &Path) -> Result<PathBuf, OrbitError> {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if !current.pop() {
            return Err(OrbitError::InvalidInput(format!(
                "linux-bwrap glob root `{}` has no existing ancestor",
                path.display()
            )));
        }
    }
    canonical_existing(&current, "glob search root")
}

/// `root` itself and everything beneath it, each path once.
fn walk_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), OrbitError> {
    out.push(root.to_path_buf());
    walk_children(root, out)
}

/// Every path beneath `root` (not `root`), each once: a directory is pushed
/// by its parent's listing, never again when it is descended into.
fn walk_children(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), OrbitError> {
    if !root.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(root).map_err(|error| {
        OrbitError::Execution(format!(
            "read linux-bwrap glob root `{}`: {error}",
            root.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!("read linux-bwrap glob entry: {error}"))
        })?;
        let path = entry.path();
        out.push(path.clone());
        if entry
            .file_type()
            .map_err(|error| {
                OrbitError::Execution(format!("inspect `{}`: {error}", path.display()))
            })?
            .is_dir()
        {
            walk_children(&path, out)?;
        }
    }
    Ok(())
}

fn canonical_existing(path: &Path, label: &str) -> Result<PathBuf, OrbitError> {
    path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "{label} `{}` must exist and resolve canonically: {error}",
            path.display()
        ))
    })
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
#[path = "tests/linux_sandbox.rs"]
mod tests;
