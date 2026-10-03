use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use orbit_common::OrbitError;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

const TRUSTED_SANDBOX_EXEC_PATHS: &[&str] = &["/usr/bin/sandbox-exec"];

pub struct MacosSandboxSpawnRequest<'a> {
    pub profile_text: &'a str,
    pub program: &'a str,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub cwd: Option<&'a Path>,
    pub stdin: Stdio,
    pub stdout: Stdio,
    pub stderr: Stdio,
    /// Descriptors the child must see at fixed numbers. `sandbox-exec` execs
    /// the confined program in its own process, so an inherited descriptor
    /// survives the wrapper — this is how a plugin backend receives its
    /// callback credential on macOS.
    pub inherited_fds: &'a [crate::process::InheritedFd],
}

pub fn spawn_under_macos_sandbox(
    request: MacosSandboxSpawnRequest<'_>,
) -> Result<(Child, Arc<NamedTempFile>), OrbitError> {
    let MacosSandboxSpawnRequest {
        profile_text,
        program,
        args,
        env,
        cwd,
        stdin,
        stdout,
        stderr,
        inherited_fds,
    } = request;

    let profile_file = cached_profile_tempfile(profile_text)?;
    let profile_path = profile_file.path().to_path_buf();

    let sandbox_exec_path = sandbox_exec_path_or_error()?;
    let mut command = Command::new(&sandbox_exec_path);
    command
        .arg("-f")
        .arg(&profile_path)
        .arg(program)
        .args(args)
        // `env` is the complete child environment the caller composed from the
        // `[execution.env]` allowlist; `sandbox-exec` hands its own environment
        // to the confined program, so nothing ambient may be seeded here.
        // [ORB-10917]
        .env_clear()
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr);
    if let Some(path) = cwd {
        command.current_dir(path);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    crate::process::attach_inherited_fds(&mut command, inherited_fds);

    let child = command.spawn().map_err(|err| {
        OrbitError::Execution(format!(
            "failed to spawn trusted sandbox-exec `{}` around `{program}`: {err}",
            sandbox_exec_path.display()
        ))
    })?;
    Ok((child, profile_file))
}

/// SHA-256 digest of a compiled SBPL profile, used as the process-wide cache
/// key in [`cached_profile_tempfile`].
type ProfileCacheKey = [u8; 32];

/// Maximum number of compiled profile tempfiles retained in the process-wide
/// cache.
///
/// Bounding cache size ensures long-lived hosts running diverse profiles do not
/// leak open file descriptors or disk files. Inactive profiles beyond this
/// capacity are evicted; if no active child process holds an [`Arc<NamedTempFile>`]
/// reference to an evicted profile, the temporary file is closed and deleted.
pub(crate) const MAX_CACHED_PROFILES: usize = 32;

/// Bounded LRU cache of compiled SBPL profile tempfiles.
struct ProfileCache {
    entries: HashMap<ProfileCacheKey, Arc<NamedTempFile>>,
    order: VecDeque<ProfileCacheKey>,
    capacity: usize,
}

impl ProfileCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, key: &ProfileCacheKey) -> Option<Arc<NamedTempFile>> {
        if let Some(file) = self.entries.get(key) {
            if let Some(pos) = self.order.iter().position(|k| k == key) {
                self.order.remove(pos);
            }
            self.order.push_back(*key);
            Some(Arc::clone(file))
        } else {
            None
        }
    }

    fn insert(&mut self, key: ProfileCacheKey, file: Arc<NamedTempFile>) {
        if self.entries.contains_key(&key) {
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
            self.order.push_back(key);
            self.entries.insert(key, file);
            return;
        }

        while self.entries.len() >= self.capacity && !self.order.is_empty() {
            if let Some(evicted_key) = self.order.pop_front() {
                self.entries.remove(&evicted_key);
            }
        }

        self.order.push_back(key);
        self.entries.insert(key, file);
    }
}

/// Process-wide reuse of compiled profile tempfiles up to [`MAX_CACHED_PROFILES`].
///
/// `(fs_profile, provider, env)` are constant within a run and across step
/// retries, so [`compile_macos_sandbox_profile`](super::compile::compile_macos_sandbox_profile)
/// emits the same SBPL text on every retry of the same activity. Without this
/// cache, each retry created, wrote, flushed, and later unlinked a fresh
/// `NamedTempFile` for text that never changed. The compiled text already
/// fully determines the enforced policy — provider-specific clauses are baked
/// into it — so hashing the text alone is a correct and sufficient key.
///
/// Evicting inactive entries beyond [`MAX_CACHED_PROFILES`] reclaims their
/// temporary files and file descriptors, preventing resource exhaustion on
/// long-lived hosts while keeping active child profiles alive via their strong
/// [`Arc<NamedTempFile>`] references.
fn profile_cache() -> &'static Mutex<ProfileCache> {
    static CACHE: OnceLock<Mutex<ProfileCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ProfileCache::new(MAX_CACHED_PROFILES)))
}

fn profile_cache_key(profile_text: &str) -> ProfileCacheKey {
    Sha256::digest(profile_text.as_bytes()).into()
}

/// Return the cached tempfile for `profile_text`, creating and writing one
/// only on the first request for that exact profile.
pub(crate) fn cached_profile_tempfile(
    profile_text: &str,
) -> Result<Arc<NamedTempFile>, OrbitError> {
    let key = profile_cache_key(profile_text);
    let mut cache = profile_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = cache.get(&key) {
        return Ok(existing);
    }

    let mut profile_file = tempfile::Builder::new()
        .prefix("orbit-sandbox-")
        .suffix(".sb")
        .tempfile()
        .map_err(|err| {
            OrbitError::Execution(format!("failed to create sandbox profile tempfile: {err}"))
        })?;
    use std::io::Write;
    profile_file
        .write_all(profile_text.as_bytes())
        .map_err(|err| {
            OrbitError::Execution(format!("failed to write sandbox profile tempfile: {err}"))
        })?;
    profile_file
        .flush()
        .map_err(|err| OrbitError::Execution(format!("failed to flush sandbox profile: {err}")))?;

    let profile_file = Arc::new(profile_file);
    cache.insert(key, Arc::clone(&profile_file));
    Ok(profile_file)
}

#[cfg(test)]
pub(crate) fn profile_cache_len() -> usize {
    profile_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
        .len()
}

#[cfg(test)]
pub(crate) static TEST_PROFILE_CACHE_LOCK: Mutex<()> = Mutex::new(());

/// Returns the stable program path used in audit logs for sandboxed CLI
/// invocations. The real spawn path is resolved again at execution time so
/// missing binaries still fail closed.
pub fn sandbox_exec_program_for_audit() -> &'static str {
    TRUSTED_SANDBOX_EXEC_PATHS[0]
}

/// Returns `true` if a trusted absolute `sandbox-exec` binary is available.
pub fn sandbox_exec_available() -> bool {
    sandbox_exec_path().is_some()
}

/// Human-facing reason used when fail-closed sandboxing cannot find the
/// trusted wrapper.
pub fn sandbox_exec_unavailable_message() -> String {
    format!(
        "trusted sandbox-exec not available at {}",
        TRUSTED_SANDBOX_EXEC_PATHS.join(", ")
    )
}

/// Resolve `sandbox-exec` from trusted absolute locations only.
pub fn sandbox_exec_path() -> Option<PathBuf> {
    sandbox_exec_path_from(TRUSTED_SANDBOX_EXEC_PATHS.iter().map(Path::new))
}

fn sandbox_exec_path_or_error() -> Result<PathBuf, OrbitError> {
    sandbox_exec_path().ok_or_else(|| OrbitError::Execution(sandbox_exec_unavailable_message()))
}

fn sandbox_exec_path_from<I, P>(candidates: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    candidates
        .into_iter()
        .map(|candidate| candidate.as_ref().to_path_buf())
        .find(|candidate| candidate.is_absolute() && is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && (meta.permissions().mode() & 0o111) != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
