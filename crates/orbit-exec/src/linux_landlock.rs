//! Linux Landlock read/execute confinement for activity-scoped `proc.spawn`.
//!
//! Argv path guessing cannot see interpreter text inside an allowed program.
//! This module applies the resolved read profile at the child process
//! boundary so a git shell alias (or any other descendant) cannot open a
//! path the profile denied.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Child;

use orbit_common::OrbitError;
use orbit_types::policy::{ResolvedFsProfile, match_glob, normalize_glob_path};

use crate::runner::ExecRequest;

const ACCESS_FS_EXECUTE: u64 = 1 << 0;
const ACCESS_FS_READ_FILE: u64 = 1 << 2;
const ACCESS_FS_READ_DIR: u64 = 1 << 3;

const HANDLED_FS_ACCESS: u64 = ACCESS_FS_EXECUTE | ACCESS_FS_READ_FILE | ACCESS_FS_READ_DIR;

const SYSTEM_DIR_ROOTS: &[&str] = &["/usr", "/bin", "/lib", "/lib64", "/sbin", "/proc", "/dev"];
const SYSTEM_FILE_ROOTS: &[&str] = &["/etc/ld.so.cache"];
const SYSTEM_OPTIONAL_DIR_ROOTS: &[&str] = &["/etc/alternatives"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockProbeOutcome {
    pub available: bool,
    pub abi: i64,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandlockAccess {
    pub read_file: bool,
    pub read_dir: bool,
    pub execute: bool,
}

impl LandlockAccess {
    fn file() -> Self {
        Self {
            read_file: true,
            read_dir: false,
            execute: true,
        }
    }

    fn directory() -> Self {
        Self {
            read_file: true,
            read_dir: true,
            execute: true,
        }
    }

    fn directory_list_only() -> Self {
        Self {
            read_file: false,
            read_dir: true,
            execute: false,
        }
    }

    fn bits(self) -> u64 {
        let mut bits = 0;
        if self.execute {
            bits |= ACCESS_FS_EXECUTE;
        }
        if self.read_file {
            bits |= ACCESS_FS_READ_FILE;
        }
        if self.read_dir {
            bits |= ACCESS_FS_READ_DIR;
        }
        bits
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPathGrant {
    pub path: PathBuf,
    pub access: LandlockAccess,
}

impl LandlockPathGrant {
    fn file(path: PathBuf) -> Self {
        Self {
            path,
            access: LandlockAccess::file(),
        }
    }

    fn directory(path: PathBuf) -> Self {
        Self {
            path,
            access: LandlockAccess::directory(),
        }
    }

    fn directory_list_only(path: PathBuf) -> Self {
        Self {
            path,
            access: LandlockAccess::directory_list_only(),
        }
    }
}

/// Probe whether this kernel accepts a Landlock ruleset.
pub fn probe_landlock() -> LandlockProbeOutcome {
    probe_landlock_impl()
}

/// Compile the path-beneath grants a confined child needs to execute and to
/// read the resolved profile, without granting host paths the profile denied.
pub fn linux_landlock_read_grants(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let workspace_root = workspace_root.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "landlock workspace `{}` must exist and resolve canonically: {error}",
            workspace_root.display()
        ))
    })?;

    let mut grants = Vec::new();
    for root in SYSTEM_DIR_ROOTS
        .iter()
        .chain(SYSTEM_OPTIONAL_DIR_ROOTS.iter())
    {
        push_existing_dir(&mut grants, Path::new(root));
    }
    for file in SYSTEM_FILE_ROOTS {
        push_existing_file(&mut grants, Path::new(file));
    }
    grants.extend(workspace_read_grants(&workspace_root, profile)?);
    Ok(dedupe_grants(grants))
}

/// Spawn `req` after restricting the child with the compiled Landlock ruleset.
pub fn spawn_under_linux_landlock(
    req: &ExecRequest,
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Child, OrbitError> {
    spawn_under_linux_landlock_impl(req, workspace_root, profile)
}

pub fn landlock_unavailable_message(probe: &LandlockProbeOutcome) -> String {
    format!(
        "activity-scoped proc.spawn requires Linux Landlock to enforce the filesystem profile ({})",
        probe.detail
    )
}

fn workspace_read_grants(
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if !profile_allows_read(profile, ".")? {
        return collect_allowed_subtrees(workspace_root, workspace_root, profile);
    }
    let denied = collect_denied_paths(workspace_root, workspace_root, profile)?;
    punch_holes(workspace_root, &denied)
}

fn collect_allowed_subtrees(
    workspace_root: &Path,
    dir: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    let mut grants = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(grants),
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!(
                "read landlock workspace `{}`: {error}",
                dir.display()
            ))
        })?;
        let Ok(path) = canonicalize_under(workspace_root, &entry.path()) else {
            continue;
        };
        let relative = workspace_relative(workspace_root, &path)?;
        if profile_allows_read(profile, &relative)? {
            let denied = collect_denied_paths(workspace_root, &path, profile)?;
            grants.extend(punch_holes(&path, &denied)?);
            continue;
        }
        if path.is_dir() {
            grants.extend(collect_allowed_subtrees(workspace_root, &path, profile)?);
        }
    }
    Ok(grants)
}

fn collect_denied_paths(
    workspace_root: &Path,
    root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<BTreeSet<PathBuf>, OrbitError> {
    let mut denied = BTreeSet::new();
    collect_denied_from(workspace_root, root, profile, &mut denied)?;
    Ok(denied)
}

fn collect_denied_from(
    workspace_root: &Path,
    dir: &Path,
    profile: &ResolvedFsProfile,
    denied: &mut BTreeSet<PathBuf>,
) -> Result<(), OrbitError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!("read landlock path `{}`: {error}", dir.display()))
        })?;
        let Ok(path) = canonicalize_under(workspace_root, &entry.path()) else {
            continue;
        };
        let relative = workspace_relative(workspace_root, &path)?;
        if !profile_allows_read(profile, &relative)? {
            denied.insert(path);
            continue;
        }
        if path.is_dir() {
            collect_denied_from(workspace_root, &path, profile, denied)?;
        }
    }
    Ok(())
}

fn punch_holes(
    root: &Path,
    denied: &BTreeSet<PathBuf>,
) -> Result<Vec<LandlockPathGrant>, OrbitError> {
    if denied.contains(root) {
        return Ok(Vec::new());
    }
    let has_denied_descendant = denied
        .iter()
        .any(|path| path.starts_with(root) && path != root);
    if !has_denied_descendant {
        return Ok(vec![full_grant(root)]);
    }
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut grants = vec![LandlockPathGrant::directory_list_only(root.to_path_buf())];
    let entries = std::fs::read_dir(root).map_err(|error| {
        OrbitError::Execution(format!(
            "read landlock hole parent `{}`: {error}",
            root.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!(
                "read landlock hole child of `{}`: {error}",
                root.display()
            ))
        })?;
        let Ok(child) = canonicalize_under(root, &entry.path()) else {
            continue;
        };
        grants.extend(punch_holes(&child, denied)?);
    }
    Ok(grants)
}

fn full_grant(path: &Path) -> LandlockPathGrant {
    if path.is_dir() {
        LandlockPathGrant::directory(path.to_path_buf())
    } else {
        LandlockPathGrant::file(path.to_path_buf())
    }
}

fn profile_allows_read(profile: &ResolvedFsProfile, relative: &str) -> Result<bool, OrbitError> {
    if profile.read.is_empty() {
        return Ok(false);
    }
    let normalized = normalize_glob_path(relative).map_err(OrbitError::from)?;
    let mut allowed = false;
    let mut matched = false;
    for rule in &profile.read {
        let (negated, pattern) = normalize_rule_pattern(rule);
        if match_glob(&pattern, &normalized).map_err(OrbitError::from)? {
            allowed = !negated;
            matched = true;
        }
    }
    Ok(matched && allowed)
}

fn normalize_rule_pattern(rule: &str) -> (bool, String) {
    let (negated, body) = split_rule(rule);
    let mut normalized = body.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if normalized.is_empty() {
        normalized = ".".to_string();
    }
    (negated, normalized)
}

fn split_rule(rule: &str) -> (bool, &str) {
    rule.strip_prefix('!')
        .map(|rest| (true, rest))
        .unwrap_or((false, rule))
}

fn workspace_relative(workspace_root: &Path, path: &Path) -> Result<String, OrbitError> {
    let relative = path.strip_prefix(workspace_root).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "landlock path `{}` escaped workspace `{}`",
            path.display(),
            workspace_root.display()
        ))
    })?;
    if relative.as_os_str().is_empty() {
        Ok(".".to_string())
    } else {
        Ok(format!(
            "./{}",
            relative.to_string_lossy().replace('\\', "/")
        ))
    }
}

fn canonicalize_under(root: &Path, path: &Path) -> Result<PathBuf, OrbitError> {
    let canonical = path.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize landlock path `{}`: {error}",
            path.display()
        ))
    })?;
    if canonical == root || canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "landlock path `{}` resolved outside `{}`",
            path.display(),
            root.display()
        )))
    }
}

fn push_existing_dir(grants: &mut Vec<LandlockPathGrant>, path: &Path) {
    if let Ok(canonical) = path.canonicalize()
        && canonical.is_dir()
    {
        grants.push(LandlockPathGrant::directory(canonical));
    }
}

fn push_existing_file(grants: &mut Vec<LandlockPathGrant>, path: &Path) {
    if let Ok(canonical) = path.canonicalize()
        && canonical.is_file()
    {
        grants.push(LandlockPathGrant::file(canonical));
    }
}

fn program_file_grants(program: &str) -> Vec<LandlockPathGrant> {
    let Some(path) = resolve_program(program) else {
        return Vec::new();
    };
    vec![LandlockPathGrant::file(path)]
}

fn resolve_program(program: &str) -> Option<PathBuf> {
    let candidate = Path::new(program);
    if candidate.is_absolute() || program.contains('/') {
        return candidate.canonicalize().ok();
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(program);
        candidate
            .is_file()
            .then(|| candidate.canonicalize().ok())
            .flatten()
    })
}

fn dedupe_grants(grants: Vec<LandlockPathGrant>) -> Vec<LandlockPathGrant> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for grant in grants {
        if seen.insert((grant.path.clone(), grant.access.bits())) {
            out.push(grant);
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn probe_landlock_impl() -> LandlockProbeOutcome {
    // SAFETY: version probe passes a null attr pointer and zero size, which
    // the Landlock ABI documents as returning the supported ABI number.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<u8>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi >= 1 {
        LandlockProbeOutcome {
            available: true,
            abi,
            detail: format!("Landlock ABI {abi}"),
        }
    } else {
        let error = std::io::Error::last_os_error();
        LandlockProbeOutcome {
            available: false,
            abi,
            detail: format!("landlock_create_ruleset version probe failed: {error}"),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_landlock_impl() -> LandlockProbeOutcome {
    LandlockProbeOutcome {
        available: false,
        abi: -1,
        detail: format!("Landlock is not implemented on {}", std::env::consts::OS),
    }
}

#[cfg(target_os = "linux")]
fn spawn_under_linux_landlock_impl(
    req: &ExecRequest,
    workspace_root: &Path,
    profile: &ResolvedFsProfile,
) -> Result<Child, OrbitError> {
    let probe = probe_landlock();
    if !probe.available {
        return Err(OrbitError::PolicyDenied(landlock_unavailable_message(
            &probe,
        )));
    }
    let mut grants = linux_landlock_read_grants(workspace_root, profile)?;
    grants.extend(program_file_grants(&req.program));
    let grants = dedupe_grants(grants);
    let ruleset = LandlockRuleset::from_grants(&grants)?;
    let mut command = crate::process::command(req);
    apply_restrict_self(&mut command, &ruleset);
    command.spawn().map_err(|error| {
        OrbitError::Execution(format!("failed to spawn `{}`: {error}", req.program))
    })
}

#[cfg(not(target_os = "linux"))]
fn spawn_under_linux_landlock_impl(
    req: &ExecRequest,
    _workspace_root: &Path,
    _profile: &ResolvedFsProfile,
) -> Result<Child, OrbitError> {
    crate::process::spawn(req)
}

#[cfg(target_os = "linux")]
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;
#[cfg(target_os = "linux")]
const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;

#[cfg(target_os = "linux")]
#[repr(C)]
struct LandlockRulesetAttr {
    handled_access_fs: u64,
}

#[cfg(target_os = "linux")]
#[repr(C, packed)]
struct LandlockPathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

#[cfg(target_os = "linux")]
struct LandlockRuleset {
    fd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl LandlockRuleset {
    fn from_grants(grants: &[LandlockPathGrant]) -> Result<Self, OrbitError> {
        let attr = LandlockRulesetAttr {
            handled_access_fs: HANDLED_FS_ACCESS,
        };
        // SAFETY: `attr` is a well-formed ruleset definition whose size matches
        // the ABI-1 `landlock_ruleset_attr` prefix.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::addr_of!(attr),
                std::mem::size_of::<LandlockRulesetAttr>(),
                0u32,
            )
        };
        if fd < 0 {
            return Err(OrbitError::PolicyDenied(format!(
                "landlock_create_ruleset failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `fd` is a newly created landlock ruleset descriptor.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) };
        let ruleset = Self { fd: owned };
        set_cloexec(ruleset.as_raw_fd())?;
        for grant in grants {
            ruleset.add_path(grant)?;
        }
        Ok(ruleset)
    }

    fn as_raw_fd(&self) -> i32 {
        std::os::fd::AsRawFd::as_raw_fd(&self.fd)
    }

    fn add_path(&self, grant: &LandlockPathGrant) -> Result<(), OrbitError> {
        use std::os::unix::ffi::OsStrExt;

        let c_path = std::ffi::CString::new(grant.path.as_os_str().as_bytes()).map_err(|_| {
            OrbitError::InvalidInput(format!(
                "landlock path `{}` contains an interior NUL",
                grant.path.display()
            ))
        })?;
        // SAFETY: `c_path` is a NUL-terminated absolute path; O_PATH | O_CLOEXEC
        // only opens a descriptor for Landlock path-beneath identity.
        let parent_fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if parent_fd < 0 {
            return Err(OrbitError::Execution(format!(
                "open landlock grant `{}`: {}",
                grant.path.display(),
                std::io::Error::last_os_error()
            )));
        }
        let mut access = grant.access.bits();
        let is_dir = grant.path.is_dir();
        if !is_dir {
            access &= !ACCESS_FS_READ_DIR;
        }
        if access == 0 {
            // SAFETY: `parent_fd` is the descriptor we opened in this function.
            unsafe {
                libc::close(parent_fd);
            }
            return Ok(());
        }
        let rule = LandlockPathBeneathAttr {
            allowed_access: access,
            parent_fd,
        };
        // SAFETY: `rule` points at a packed path-beneath attr whose `parent_fd`
        // is the live O_PATH descriptor opened above.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                self.as_raw_fd() as libc::c_long,
                LANDLOCK_RULE_PATH_BENEATH as libc::c_long,
                std::ptr::addr_of!(rule),
                0u32,
            )
        };
        // SAFETY: `parent_fd` is the descriptor we opened in this function.
        unsafe {
            libc::close(parent_fd);
        }
        if added != 0 {
            return Err(OrbitError::PolicyDenied(format!(
                "landlock_add_rule failed for `{}`: {}",
                grant.path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn set_cloexec(fd: i32) -> Result<(), OrbitError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(OrbitError::Execution(format!(
            "fcntl F_GETFD on landlock ruleset: {}",
            std::io::Error::last_os_error()
        )));
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result < 0 {
        return Err(OrbitError::Execution(format!(
            "fcntl F_SETFD FD_CLOEXEC on landlock ruleset: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn apply_restrict_self(command: &mut std::process::Command, ruleset: &LandlockRuleset) {
    use std::os::unix::process::CommandExt;

    let fd = ruleset.as_raw_fd();
    // SAFETY: the closure only issues async-signal-safe syscalls on the
    // inherited ruleset fd after fork and before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let restricted =
                libc::syscall(libc::SYS_landlock_restrict_self, fd as libc::c_long, 0u32);
            if restricted != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd;

#[cfg(test)]
#[path = "tests/linux_landlock.rs"]
mod tests;
