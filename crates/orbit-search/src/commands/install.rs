#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{Read, Seek, Write};
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::security::release::{
    RELEASE_CHECKSUMS_FILENAME, RELEASE_CHECKSUMS_SIGNATURE_FILENAME, TRUSTED_RELEASE_KEYS,
};
use reqwest::{Url, blocking::Client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::commands::{DEFAULT_RELEASE_BASE_URL, parse_model};
use crate::companion::{
    COMPANION_OVERRIDE_ENV, UNSAFE_COMPANION_OVERRIDE_ENV, ensure_semantic_search_supported,
    unsafe_companion_overrides_enabled, validate_companion_override_path,
    validate_managed_companion_path,
};
use crate::{CompanionPaths, platform_companion_filename};

const COMPANION_URL_ENV: &str = "ORBIT_SEARCH_COMPANION_URL";
const COMPANION_SHA256_ENV: &str = "ORBIT_SEARCH_COMPANION_SHA256";
const COMPANION_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest silence tolerated on an accepted companion connection. This is an
/// idle bound, not a transfer deadline: see [`companion_download_client`].
pub(crate) const COMPANION_STALLED_READ_TIMEOUT: Duration = Duration::from_secs(30);
const CHECKSUM_METADATA_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct SemanticInstallParams {
    pub model: Option<String>,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticInstallResult {
    pub companion_path: String,
    pub companion_changed: bool,
    pub model_id: String,
    pub model_installed: bool,
}

pub fn run(params: SemanticInstallParams) -> Result<SemanticInstallResult, OrbitError> {
    let spec = parse_model(params.model.as_deref())?;
    let paths = CompanionPaths::default_under_home()?;
    fs::create_dir_all(&paths.bin_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
    fs::create_dir_all(&paths.models_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
    emit_stale_companion_hint(&paths);

    let companion_path = paths.companion_path();
    let current_companion = if params.force {
        None
    } else {
        match ManagedCompanion::open_current(&companion_path) {
            Ok(companion) => Some(companion),
            Err(error) => {
                // Covers integrity mismatch, missing manifest, path validation
                // failures, and genuine I/O errors. We treat all of them as
                // "needs reinstall," but record the cause so debugging "why
                // did this just reinstall" doesn't require re-running with a
                // patched build.
                tracing::debug!(
                    companion_path = %companion_path.display(),
                    error = %error,
                    "managed companion failed integrity or open check; treating as needs-reinstall"
                );
                None
            }
        }
    };
    let (companion_changed, companion) = if let Some(companion) = current_companion {
        (false, companion)
    } else {
        install_companion(&companion_path)?;
        (true, ManagedCompanion::open_current(&companion_path)?)
    };

    let model_dir = paths.model_dir(spec.alias);
    let marker_path = model_dir.join("orbit-model.json");
    let model_installed = if marker_path.exists() {
        false
    } else {
        fs::create_dir_all(&model_dir).map_err(|error| OrbitError::Io(error.to_string()))?;
        companion.download_model(spec.alias, &model_dir)?;
        true
    };
    fs::write(&paths.active_model_path, spec.alias)
        .map_err(|error| OrbitError::Io(error.to_string()))?;

    Ok(SemanticInstallResult {
        companion_path: companion_path.to_string_lossy().to_string(),
        companion_changed,
        model_id: spec.alias.to_string(),
        model_installed,
    })
}

fn install_companion(destination: &Path) -> Result<(), OrbitError> {
    let temp_path = temporary_companion_path(destination)?;
    if temp_path.exists() {
        fs::remove_file(&temp_path).map_err(|error| OrbitError::Io(error.to_string()))?;
    }

    let install_result = install_companion_to_temp(&temp_path).and_then(|checksum| {
        replace_companion(&temp_path, destination)?;
        write_companion_integrity(destination, &checksum)
    });
    if install_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    install_result
}

pub(crate) fn install_companion_to_temp(temp_path: &Path) -> Result<String, OrbitError> {
    if let Some(local_path) = env_var_non_empty(COMPANION_OVERRIDE_ENV) {
        return install_local_companion(Path::new(&local_path), temp_path);
    }

    let source = resolve_download_source()?;
    let client = companion_download_client()?;
    let checksum = download_companion_to_temp(&client, &source.url, temp_path)?;
    verify_download_integrity(&checksum, &source.integrity, &client)?;
    make_executable(temp_path)?;
    Ok(checksum)
}

fn install_local_companion(source_path: &Path, temp_path: &Path) -> Result<String, OrbitError> {
    validate_companion_override_path(source_path)?;
    let bytes = fs::read(source_path).map_err(|error| OrbitError::Io(error.to_string()))?;
    let checksum = sha256_hex(&bytes);
    if let Some(expected) = env_var_non_empty(COMPANION_SHA256_ENV) {
        verify_sha256_digest(&checksum, &expected)?;
    }
    fs::write(temp_path, bytes).map_err(|error| OrbitError::Io(error.to_string()))?;
    make_executable(temp_path)?;
    Ok(checksum)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompanionDownloadSource {
    pub(crate) url: String,
    pub(crate) integrity: CompanionIntegrity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompanionIntegrity {
    ReleaseSignedChecksum {
        checksums_url: String,
        signature_url: String,
        asset_name: String,
    },
    Sha256(String),
    UnsafeDeveloperOverride,
}

pub(crate) fn resolve_download_source() -> Result<CompanionDownloadSource, OrbitError> {
    if let Some(url) = env_var_non_empty(COMPANION_URL_ENV) {
        validate_download_url(&url)?;
        if let Some(expected) = env_var_non_empty(COMPANION_SHA256_ENV) {
            return Ok(CompanionDownloadSource {
                url,
                integrity: CompanionIntegrity::Sha256(normalize_sha256(&expected)?),
            });
        }
        if unsafe_companion_overrides_enabled() {
            tracing::warn!(
                env_var = UNSAFE_COMPANION_OVERRIDE_ENV,
                url = %url,
                "unsafe companion download bypasses checksum verification"
            );
            return Ok(CompanionDownloadSource {
                url,
                integrity: CompanionIntegrity::UnsafeDeveloperOverride,
            });
        }
        return Err(OrbitError::InvalidInput(format!(
            "{COMPANION_URL_ENV} requires {COMPANION_SHA256_ENV}=<sha256>; set {UNSAFE_COMPANION_OVERRIDE_ENV}=1 only for developer-only unsigned downloads"
        )));
    }

    ensure_semantic_search_supported()?;
    default_release_download_source(platform_companion_filename())
}

pub(crate) fn default_release_download_source(
    asset_name: String,
) -> Result<CompanionDownloadSource, OrbitError> {
    let url = format!("{DEFAULT_RELEASE_BASE_URL}/{asset_name}");
    validate_download_url(&url)?;
    Ok(CompanionDownloadSource {
        url,
        integrity: CompanionIntegrity::ReleaseSignedChecksum {
            checksums_url: release_metadata_url(RELEASE_CHECKSUMS_FILENAME),
            signature_url: release_metadata_url(RELEASE_CHECKSUMS_SIGNATURE_FILENAME),
            asset_name,
        },
    })
}

fn release_metadata_url(filename: &str) -> String {
    format!(
        "{}/{}",
        DEFAULT_RELEASE_BASE_URL.trim_end_matches('/'),
        filename
    )
}

fn validate_download_url(url: &str) -> Result<(), OrbitError> {
    let parsed = Url::parse(url)
        .map_err(|error| OrbitError::InvalidInput(format!("invalid companion URL: {error}")))?;
    if parsed.scheme() != "https" {
        if unsafe_companion_overrides_enabled() {
            tracing::warn!(
                env_var = UNSAFE_COMPANION_OVERRIDE_ENV,
                url,
                scheme = parsed.scheme(),
                "unsafe companion download bypasses HTTPS enforcement"
            );
        } else {
            return Err(OrbitError::InvalidInput(format!(
                "companion downloads must use https; set {UNSAFE_COMPANION_OVERRIDE_ENV}=1 only for developer-only testing"
            )));
        }
    }
    Ok(())
}

/// Builds the companion download client.
///
/// Companion assets are large enough that a whole-request deadline rejects
/// healthy slow links, so the client carries no transfer deadline. reqwest's
/// blocking client re-arms `timeout` for every individual operation — the send
/// phase, then each [`std::io::Read::read`] on the streamed response — so the
/// bound below only fires when a connection goes silent, and resets after each
/// chunk that arrives. Reading a companion body must therefore stay on the
/// streaming loop in [`download_companion_to_temp`]; the buffering accessors
/// (`Response::bytes`, `text`, `json`) apply the same value once to the whole
/// body, which is exactly the total deadline this must not reintroduce.
pub(crate) fn companion_download_client() -> Result<Client, OrbitError> {
    Client::builder()
        .timeout(COMPANION_STALLED_READ_TIMEOUT)
        .connect_timeout(COMPANION_CONNECT_TIMEOUT)
        .build()
        .map_err(|error| {
            OrbitError::Execution(format!("failed to configure companion download: {error}"))
        })
}

pub(crate) fn download_companion_to_temp(
    client: &Client,
    url: &str,
    temp_path: &Path,
) -> Result<String, OrbitError> {
    let mut response = client
        .get(url)
        .send()
        .map_err(|error| OrbitError::Execution(format!("failed to download companion: {error}")))?
        .error_for_status()
        .map_err(|error| OrbitError::Execution(format!("failed to download companion: {error}")))?;
    let mut file =
        fs::File::create(temp_path).map_err(|error| OrbitError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];

    loop {
        let count = response.read(&mut buffer).map_err(|error| {
            OrbitError::Execution(format!("failed to read companion download: {error}"))
        })?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn download_checksum_manifest(client: &Client, url: &str) -> Result<Vec<u8>, OrbitError> {
    Ok(client
        .get(url)
        .timeout(CHECKSUM_METADATA_TIMEOUT)
        .send()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to download companion checksum manifest: {error}"
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to download companion checksum manifest: {error}"
            ))
        })?
        .bytes()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to read companion checksum manifest: {error}"
            ))
        })?
        .to_vec())
}

fn download_checksum_signature(client: &Client, url: &str) -> Result<Vec<u8>, OrbitError> {
    Ok(client
        .get(url)
        .timeout(CHECKSUM_METADATA_TIMEOUT)
        .send()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to download companion checksum signature: {error}"
            ))
        })?
        .error_for_status()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to download companion checksum signature: {error}"
            ))
        })?
        .bytes()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to read companion checksum signature: {error}"
            ))
        })?
        .to_vec())
}

fn verify_download_integrity(
    checksum: &str,
    integrity: &CompanionIntegrity,
    client: &Client,
) -> Result<String, OrbitError> {
    match integrity {
        CompanionIntegrity::ReleaseSignedChecksum {
            checksums_url,
            signature_url,
            asset_name,
        } => {
            let manifest = download_checksum_manifest(client, checksums_url)?;
            let signature = download_checksum_signature(client, signature_url)?;
            verify_release_checksum_signature(&manifest, &signature)?;
            let manifest = std::str::from_utf8(&manifest).map_err(|error| {
                OrbitError::Execution(format!("companion checksum manifest is not UTF-8: {error}"))
            })?;
            let expected = checksum_from_manifest(manifest, asset_name)?;
            verify_sha256_digest(checksum, &expected)?;
        }
        CompanionIntegrity::Sha256(expected) => verify_sha256_digest(checksum, expected)?,
        CompanionIntegrity::UnsafeDeveloperOverride => {}
    }
    Ok(checksum.to_string())
}

fn verify_release_checksum_signature(manifest: &[u8], signature: &[u8]) -> Result<(), OrbitError> {
    // L-0044: the trusted key set and the verification itself belong to
    // `orbit_common::security::release`, shared with `orbit update`, so a
    // rotation is one edit rather than one per release-artifact consumer.
    orbit_common::security::release::verify_checksum_signature(
        manifest,
        signature,
        TRUSTED_RELEASE_KEYS,
        chrono::Utc::now().date_naive(),
    )
    .map(|_key_id| ())
}

pub(crate) fn checksum_from_manifest(
    manifest: &str,
    asset_name: &str,
) -> Result<String, OrbitError> {
    orbit_common::security::release::checksum_for_asset(manifest, asset_name)
}

fn verify_sha256_digest(actual: &str, expected: &str) -> Result<(), OrbitError> {
    orbit_common::security::release::verify_sha256_digest(actual, expected, "companion")
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    orbit_common::security::release::sha256_hex(bytes)
}

fn normalize_sha256(value: &str) -> Result<String, OrbitError> {
    orbit_common::security::release::normalize_sha256(value, COMPANION_SHA256_ENV)
}

fn env_var_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn emit_stale_companion_hint(paths: &CompanionPaths) {
    let stale_path = paths.bin_dir.join(legacy_platform_companion_filename());
    if stale_path.exists() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "stale companion detected at {}; remove it or run `orbit semantic install --force`",
            stale_path.display()
        );
    }
}

fn legacy_platform_companion_filename() -> String {
    let base = concat!("orbit-", "embed", "-companion");
    if cfg!(windows) {
        format!("{base}-{}.exe", crate::platform_id())
    } else {
        format!("{base}-{}", crate::platform_id())
    }
}

#[derive(Debug)]
pub(crate) struct ManagedCompanion {
    path: PathBuf,
    file: fs::File,
}

impl ManagedCompanion {
    pub(crate) fn open_current(path: &Path) -> Result<Self, OrbitError> {
        validate_managed_companion_path(path)?;
        let file = fs::File::open(path).map_err(|error| OrbitError::Io(error.to_string()))?;
        let companion = Self {
            path: path.to_path_buf(),
            file,
        };
        // L-0036: Avoid native version probes; the sidecar lets us decide "install needed"
        // without executing an untrusted binary, but is not a tamper-detection mechanism.
        if !installed_companion_integrity_matches(&companion)? {
            return Err(OrbitError::Execution(format!(
                "installed search companion integrity metadata is stale for {}",
                path.display()
            )));
        }
        Ok(companion)
    }

    pub(crate) fn descriptor_checksum(&self) -> Result<String, OrbitError> {
        sha256_file(&self.file)
    }

    pub(crate) fn download_model(&self, model: &str, model_dir: &Path) -> Result<(), OrbitError> {
        match companion_launch_mode() {
            CompanionLaunchMode::FileDescriptor => {
                #[cfg(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd",
                    target_os = "dragonfly"
                ))]
                {
                    download_model_with_companion_fd(self, model, model_dir)
                }
                #[cfg(not(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd",
                    target_os = "dragonfly"
                )))]
                {
                    unreachable!("file-descriptor launch mode is unavailable on this target")
                }
            }
            CompanionLaunchMode::Path => {
                #[cfg(not(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd",
                    target_os = "dragonfly"
                )))]
                tracing::debug!(
                    companion_path = %self.path.display(),
                    reason = path_execution_fallback_rationale(),
                    "executing managed search companion by path"
                );
                download_model_with_companion_path(&self.path, model, model_dir)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompanionLaunchMode {
    FileDescriptor,
    Path,
}

pub(crate) fn companion_launch_mode() -> CompanionLaunchMode {
    if cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )) {
        CompanionLaunchMode::FileDescriptor
    } else {
        CompanionLaunchMode::Path
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
)))]
pub(crate) fn path_execution_fallback_rationale() -> &'static str {
    // Named explicitly so release notes and operators are unambiguous about
    // which platforms still carry the original ORB-00271 TOCTOU window.
    // macOS and Windows are the practical impact set: macOS ships no stable
    // libc fexecve, and Windows has no equivalent fd-exec primitive at all.
    // Descriptor-based freshness validation runs on every platform, but the
    // model-download exec on these targets still goes through the path, so
    // a process with write access to ~/.orbit/embed/bin/ between freshness
    // check and exec can still substitute the binary. Tracked for posix_spawn
    // /dev/fd/N exploration as a follow-up to ORB-00271."
    "this platform (notably macOS, plus Windows) does not expose fexecve through libc, so the managed companion keeps the pre-existing path execution behavior after descriptor-based freshness validation; the descriptor-vs-path TOCTOU window remains open on these targets"
}

#[derive(Debug, Deserialize, Serialize)]
struct CompanionIntegrityManifest {
    version: String,
    sha256: String,
}

fn installed_companion_integrity_matches(companion: &ManagedCompanion) -> Result<bool, OrbitError> {
    let manifest_path = companion_integrity_path(&companion.path).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "companion destination has no file name: {}",
            companion.path.display()
        ))
    })?;
    let manifest =
        fs::read_to_string(manifest_path).map_err(|error| OrbitError::Io(error.to_string()))?;
    let manifest: CompanionIntegrityManifest =
        serde_json::from_str(&manifest).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "companion integrity manifest is not valid JSON: {error}"
            ))
        })?;
    let checksum = companion.descriptor_checksum()?;
    Ok(manifest.version == env!("CARGO_PKG_VERSION")
        && normalize_sha256(&manifest.sha256)? == checksum)
}

fn sha256_file(file: &fs::File) -> Result<String, OrbitError> {
    let mut reader = file
        .try_clone()
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn write_companion_integrity(path: &Path, checksum: &str) -> Result<(), OrbitError> {
    let manifest_path = companion_integrity_path(path).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "companion destination has no file name: {}",
            path.display()
        ))
    })?;
    let content = serde_json::to_string_pretty(&CompanionIntegrityManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        sha256: checksum.to_string(),
    })
    .map(|json| format!("{json}\n"))
    .map_err(|error| {
        OrbitError::Execution(format!(
            "failed to serialize companion integrity manifest: {error}"
        ))
    })?;
    fs::write(manifest_path, content).map_err(|error| OrbitError::Io(error.to_string()))
}

pub(crate) fn companion_integrity_path(path: &Path) -> Option<std::path::PathBuf> {
    let file_name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{file_name}.sha256")))
}

fn temporary_companion_path(destination: &Path) -> Result<std::path::PathBuf, OrbitError> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "companion destination has no file name: {}",
                destination.display()
            ))
        })?;
    Ok(destination.with_file_name(format!(".{file_name}.tmp-{}", std::process::id())))
}

#[cfg(unix)]
fn replace_companion(temp_path: &Path, destination: &Path) -> Result<(), OrbitError> {
    fs::rename(temp_path, destination).map_err(|error| OrbitError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn replace_companion(temp_path: &Path, destination: &Path) -> Result<(), OrbitError> {
    if destination.exists() {
        fs::remove_file(destination).map_err(|error| OrbitError::Io(error.to_string()))?;
    }
    fs::rename(temp_path, destination).map_err(|error| OrbitError::Io(error.to_string()))
}

fn download_model_with_companion_path(
    companion_path: &Path,
    model: &str,
    model_dir: &Path,
) -> Result<(), OrbitError> {
    let status = Command::new(companion_path)
        .arg("--model")
        .arg(model)
        .arg("--model-path")
        .arg(model_dir)
        .arg("--download-model")
        .status()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to run search companion for model download: {error}"
            ))
        })?;
    if !status.success() {
        return Err(OrbitError::Execution(format!(
            "search companion failed to download model `{model}`"
        )));
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn download_model_with_companion_fd(
    companion: &ManagedCompanion,
    model: &str,
    model_dir: &Path,
) -> Result<(), OrbitError> {
    if !run_companion_fd_for_model_download(&companion.file, &companion.path, model, model_dir)? {
        return Err(OrbitError::Execution(format!(
            "search companion failed to download model `{model}`"
        )));
    }
    Ok(())
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn run_companion_fd_for_model_download(
    file: &fs::File,
    companion_path: &Path,
    model: &str,
    model_dir: &Path,
) -> Result<bool, OrbitError> {
    use std::os::fd::AsRawFd;

    let argv = companion_argv(companion_path, model, model_dir)?;
    let mut argv_ptrs = argv.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
    argv_ptrs.push(std::ptr::null());
    let env = companion_env()?;
    let mut env_ptrs = env.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    env_ptrs.push(std::ptr::null());
    let fd = file.as_raw_fd();

    let pid = {
        // SAFETY: fork() is invoked with no Rust-side preconditions, but the
        // standard multi-threaded fork hazard applies: after fork() the child
        // inherits only the calling thread, so any state owned by other
        // threads (allocator mutexes, tracing dispatch locks, tokio runtime
        // state, etc.) is in an indeterminate state in the child. We rely on
        // `orbit semantic install` being invoked from a synchronous CLI path
        // (no tokio runtime active on this thread, no Rust-side locks held by
        // other threads at this point). The child path only calls
        // async-signal-safe libc primitives (fexecve, _exit) on buffers
        // (argv_ptrs, env_ptrs, fd) prepared in the parent before fork, so
        // it never re-enters Rust drop glue or the allocator. If a future
        // caller introduces a runtime here, swap this for posix_spawn — the
        // /dev/fd/<N> path is the equivalent and portable fix.
        unsafe { libc::fork() }
    };
    if pid < 0 {
        return Err(OrbitError::Execution(format!(
            "failed to start search companion for model download: {}",
            std::io::Error::last_os_error()
        )));
    }
    if pid == 0 {
        // SAFETY: fd references the opened companion file, argv/envp are
        // null-terminated pointer arrays whose CString storage is alive across fork.
        // The child clears FD_CLOEXEC before fexecve so Linux can execute a
        // shebang script descriptor through its interpreter; the parent keeps
        // its original descriptor flags because fork copied the fd table.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags == -1 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                libc::_exit(126);
            }
            libc::fexecve(fd, argv_ptrs.as_ptr(), env_ptrs.as_ptr());
            libc::_exit(127);
        }
    }
    wait_for_companion(pid)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn companion_argv(
    companion_path: &Path,
    model: &str,
    model_dir: &Path,
) -> Result<Vec<CString>, OrbitError> {
    Ok(vec![
        cstring_from_os(companion_path.as_os_str(), "companion path")?,
        CString::new("--model").map_err(|error| OrbitError::InvalidInput(error.to_string()))?,
        CString::new(model).map_err(|error| {
            OrbitError::InvalidInput(format!("model identifier contains a NUL byte: {error}"))
        })?,
        CString::new("--model-path")
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?,
        cstring_from_os(model_dir.as_os_str(), "model path")?,
        CString::new("--download-model")
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?,
    ])
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn companion_env() -> Result<Vec<CString>, OrbitError> {
    std::env::vars_os()
        .map(|(name, value)| {
            let mut entry = name.as_os_str().as_bytes().to_vec();
            entry.push(b'=');
            entry.extend(value.as_os_str().as_bytes());
            CString::new(entry).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "environment contains a NUL byte and cannot be passed to the companion: {error}"
                ))
            })
        })
        .collect()
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn cstring_from_os(value: &OsStr, label: &str) -> Result<CString, OrbitError> {
    CString::new(value.as_bytes())
        .map_err(|error| OrbitError::InvalidInput(format!("{label} contains a NUL byte: {error}")))
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn wait_for_companion(pid: libc::pid_t) -> Result<bool, OrbitError> {
    let mut status = 0;
    loop {
        let waited = {
            // SAFETY: waitpid is called for the child pid returned by fork with a valid
            // pointer to collect its status.
            unsafe { libc::waitpid(pid, &mut status, 0) }
        };
        if waited == pid {
            break;
        }
        let error = std::io::Error::last_os_error();
        if waited == -1 && error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return Err(OrbitError::Execution(format!(
            "failed to wait for search companion: {error}"
        )));
    }
    Ok(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|error| OrbitError::Io(error.to_string()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|error| OrbitError::Io(error.to_string()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}
