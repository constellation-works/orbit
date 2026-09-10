//! Discovery of the installed search companion binary.
//!
//! `CompanionPaths` describes the on-disk layout under `~/.orbit/embed/`,
//! and `locate_companion()` resolves a callable path by checking, in order,
//! the standard install location, then a gated `ORBIT_SEARCH_COMPANION`
//! developer override. When both miss, the error is the actionable
//! `CompanionNotInstalled` shape so callers can surface a clean install hint.

use std::env;
#[cfg(all(target_os = "linux", target_env = "gnu"))]
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use orbit_common::OrbitError;

pub(crate) const COMPANION_OVERRIDE_ENV: &str = "ORBIT_SEARCH_COMPANION";
pub(crate) const UNSAFE_COMPANION_OVERRIDE_ENV: &str = "ORBIT_SEARCH_COMPANION_ALLOW_UNSAFE";
pub const INSTALL_REMEDIATION: &str = "Semantic search not enabled. Run `orbit semantic install` to download the inference companion.";
const MIN_LINUX_GLIBC_MAJOR: u32 = 2;
const MIN_LINUX_GLIBC_MINOR: u32 = 38;
const MIN_LINUX_GLIBC_DISPLAY: &str = "2.38";

#[derive(Debug, Clone)]
pub struct CompanionPaths {
    pub root: PathBuf,
    pub bin_dir: PathBuf,
    pub models_dir: PathBuf,
    pub active_model_path: PathBuf,
}

impl CompanionPaths {
    pub fn default_under_home() -> Result<Self, OrbitError> {
        let root = home_dir()
            .ok_or_else(|| OrbitError::InvalidInput("HOME/USERPROFILE is not set".to_string()))?
            .join(".orbit")
            .join("embed");
        Ok(Self::new(root))
    }

    pub fn new(root: PathBuf) -> Self {
        Self {
            bin_dir: root.join("bin"),
            models_dir: root.join("models"),
            active_model_path: root.join("active-model"),
            root,
        }
    }

    pub fn companion_path(&self) -> PathBuf {
        self.bin_dir.join(platform_companion_filename())
    }

    pub fn model_dir(&self, model_id: &str) -> PathBuf {
        self.models_dir.join(model_id)
    }
}

pub fn platform_companion_filename() -> String {
    if cfg!(windows) {
        format!("orbit-search-companion-{}.exe", platform_id())
    } else {
        format!("orbit-search-companion-{}", platform_id())
    }
}

pub fn platform_id() -> &'static str {
    static PLATFORM_ID: OnceLock<String> = OnceLock::new();

    PLATFORM_ID
        .get_or_init(|| platform_id_for(env::consts::OS, env::consts::ARCH))
        .as_str()
}

fn platform_id_for(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("macos", "aarch64")
        | ("macos", "x86_64")
        | ("linux", "aarch64")
        | ("linux", "x86_64")
        | ("windows", "x86_64") => format!("{os}-{arch}"),
        _ => "unknown".to_owned(),
    }
}

pub(crate) fn ensure_semantic_search_supported() -> Result<(), OrbitError> {
    let glibc_version = current_glibc_version();
    ensure_semantic_search_supported_for_platform(platform_id(), glibc_version.as_deref())
}

pub(crate) fn ensure_semantic_search_supported_for_platform(
    platform: &str,
    glibc_version: Option<&str>,
) -> Result<(), OrbitError> {
    match platform {
        "macos-aarch64" => Ok(()),
        "linux-aarch64" | "linux-x86_64" => ensure_linux_glibc_supported(platform, glibc_version),
        _ => Err(unsupported_semantic_platform(platform)),
    }
}

fn ensure_linux_glibc_supported(
    platform: &str,
    glibc_version: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(glibc_version) = glibc_version else {
        return Ok(());
    };
    let Some((major, minor)) = parse_glibc_major_minor(glibc_version) else {
        return Ok(());
    };
    if (major, minor) < (MIN_LINUX_GLIBC_MAJOR, MIN_LINUX_GLIBC_MINOR) {
        return Err(OrbitError::InvalidInput(format!(
            "semantic search unsupported on this Linux system ({platform}): orbit-search-companion requires glibc >= {MIN_LINUX_GLIBC_DISPLAY}; detected glibc {glibc_version}. Use Ubuntu 24.04 or another Linux distribution with glibc >= {MIN_LINUX_GLIBC_DISPLAY}."
        )));
    }
    Ok(())
}

fn unsupported_semantic_platform(platform: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "semantic search unsupported on this platform ({platform}); supported companion platforms are macOS arm64 and Linux x86_64/aarch64 with glibc >= {MIN_LINUX_GLIBC_DISPLAY}"
    ))
}

fn parse_glibc_major_minor(version: &str) -> Option<(u32, u32)> {
    let mut pieces = version.split('.');
    let major = pieces.next()?.parse::<u32>().ok()?;
    let minor = pieces.next()?.parse::<u32>().ok()?;
    Some((major, minor))
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn current_glibc_version() -> Option<String> {
    let version = {
        // SAFETY: gnu_get_libc_version returns a process-static NUL-terminated
        // string pointer, or null on failure; we only read it when non-null.
        unsafe { libc::gnu_get_libc_version() }
    };
    if version.is_null() {
        return None;
    }
    let version = {
        // SAFETY: non-null gnu_get_libc_version result points at a valid C string.
        unsafe { CStr::from_ptr(version) }
    };
    version.to_str().ok().map(str::to_string)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn current_glibc_version() -> Option<String> {
    None
}

pub fn locate_companion() -> Result<PathBuf, OrbitError> {
    if let Ok(paths) = CompanionPaths::default_under_home() {
        let standard = paths.companion_path();
        if is_executable_file(&standard) {
            return Ok(standard);
        }
    }

    if let Ok(path) = env::var(COMPANION_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        validate_companion_override_path(&path)?;
        return Ok(path);
    }

    Err(OrbitError::CompanionNotInstalled(
        INSTALL_REMEDIATION.to_string(),
    ))
}

pub(crate) fn unsafe_companion_overrides_enabled() -> bool {
    env::var(UNSAFE_COMPANION_OVERRIDE_ENV)
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

pub(crate) fn validate_companion_override_path(path: &Path) -> Result<(), OrbitError> {
    if !unsafe_companion_overrides_enabled() {
        return Err(OrbitError::InvalidInput(format!(
            "{COMPANION_OVERRIDE_ENV} is a developer-only override; set {UNSAFE_COMPANION_OVERRIDE_ENV}=1 after verifying the companion path is trusted"
        )));
    }
    tracing::warn!(
        env_var = UNSAFE_COMPANION_OVERRIDE_ENV,
        override_env = COMPANION_OVERRIDE_ENV,
        path = %path.display(),
        "unsafe companion lookup bypasses managed-install-only execution"
    );
    if !path.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "{COMPANION_OVERRIDE_ENV} must be an absolute path: {}",
            path.display()
        )));
    }
    validate_companion_path(path, CompanionPathTrust::DeveloperOverride)
}

pub(crate) fn validate_managed_companion_path(path: &Path) -> Result<(), OrbitError> {
    validate_companion_path(path, CompanionPathTrust::ManagedInstall)
}

fn is_executable_file(path: &Path) -> bool {
    validate_managed_companion_path(path).is_ok()
}

#[derive(Debug, Clone, Copy)]
enum CompanionPathTrust {
    ManagedInstall,
    DeveloperOverride,
}

fn validate_companion_path(path: &Path, trust: CompanionPathTrust) -> Result<(), OrbitError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "search companion path is not readable at {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "search companion path must be a regular file: {}",
            path.display()
        )));
    }
    validate_executable_metadata(path, &metadata)?;
    if matches!(trust, CompanionPathTrust::DeveloperOverride) {
        validate_developer_override_metadata(path, &metadata)?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_executable_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "search companion is not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_executable_metadata(_path: &Path, _metadata: &fs::Metadata) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(unix)]
fn validate_developer_override_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), OrbitError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let effective_uid = {
        // SAFETY: geteuid has no preconditions and only reads the process effective uid.
        unsafe { libc::geteuid() }
    };
    if metadata.uid() != effective_uid {
        return Err(OrbitError::InvalidInput(format!(
            "search companion override must be owned by the current user: {}",
            path.display()
        )));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(OrbitError::InvalidInput(format!(
            "search companion override must not be group/world writable: {}",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "search companion override has no parent directory: {}",
            path.display()
        ))
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "search companion override parent is not readable at {}: {error}",
            parent.display()
        ))
    })?;
    if parent_metadata.uid() != effective_uid {
        return Err(OrbitError::InvalidInput(format!(
            "search companion override parent must be owned by the current user: {}",
            parent.display()
        )));
    }
    if parent_metadata.permissions().mode() & 0o022 != 0 {
        return Err(OrbitError::InvalidInput(format!(
            "search companion override parent must not be group/world writable: {}",
            parent.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_developer_override_metadata(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), OrbitError> {
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
        })
}
