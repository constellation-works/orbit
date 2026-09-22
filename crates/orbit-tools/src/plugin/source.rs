//! Resolve an `orbit plugin add` source to a local directory.
//!
//! Four forms (§3): a local directory, `git+<url>[#<ref>]`, a local
//! `.tar.gz`/`.tgz`/`.tar`/`.zip` archive, and an `https://` archive Orbit
//! fetches itself. Fetching runs here rather than in Core because this is the
//! crate that owns spawning a process.
//!
//! A fetched archive is the only source whose bytes Orbit chooses to pull
//! over the network, so it is the only one that carries a mandatory
//! [`PluginSourceRequest::expected_digest`]: the workspace declares the
//! SHA-256 it expects and an archive that hashes to anything else is refused.
//! There is deliberately no trust-on-first-use fallback — the first fetch is
//! exactly the one whoever controls the URL would tamper with.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_common::security::release::sha256_hex;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use orbit_types::plugin::{parse_archive_digest, remote_archive_source};

use super::loader::{plugin_symlink_refusal, refuse_plugin_tree_symlinks};
use crate::TIMEOUT_LONG_MS;

/// Largest archive Orbit will read for a plugin source. A plugin tree is a
/// manifest, its schemas and a backend; anything past this is not one.
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest tree an archive may unpack to. This, not the download bound, is
/// what makes a compression bomb harmless: a few compressed kilobytes say
/// nothing about how many bytes they inflate to.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Most members one archive may carry, so an archive of millions of empty
/// files cannot exhaust inodes before the size bound ever trips.
const MAX_ARCHIVE_ENTRIES: usize = 20_000;

/// The bounds one unpack runs under.
///
/// Carried as a value rather than read from the constants at each use so the
/// two container implementations cannot drift apart, and so a test can state
/// a small bound instead of having to build a 256 MiB archive to reach the
/// shipped one.
#[derive(Debug, Clone, Copy)]
pub(super) struct ArchiveLimits {
    /// Total bytes an archive may unpack to.
    pub(super) unpacked_bytes: u64,
    /// Number of members an archive may carry.
    pub(super) entries: usize,
}

impl ArchiveLimits {
    /// What every install unpacks under.
    pub(super) const DEFAULT: Self = Self {
        unpacked_bytes: MAX_UNPACKED_BYTES,
        entries: MAX_ARCHIVE_ENTRIES,
    };
}

/// Most redirects the fetch follows, every hop of which must stay on HTTPS.
const MAX_REDIRECTS: u32 = 5;

/// Where a resolved source's tree lives, and what the source was.
#[derive(Debug)]
pub struct ResolvedSource {
    /// Directory holding `plugin.yaml`.
    pub root: PathBuf,
    /// Set when the tree was fetched into scratch; dropping it removes the
    /// scratch directory, so the caller holds it until the copy is done.
    pub scratch: Option<tempfile::TempDir>,
    /// SHA-256 of the archive this was fetched from, hex encoded, when the
    /// source was an `https://` archive. `None` for every source Orbit did
    /// not download.
    pub archive_digest: Option<String>,
}

/// What to resolve, plus the digest a fetched archive has to hash to.
#[derive(Debug, Clone, Copy)]
pub struct PluginSourceRequest<'a> {
    /// The source as the operator or the pin file wrote it.
    pub source: &'a str,
    /// `sha256:<hex>` an `https://` archive must hash to. `None` refuses such
    /// a source outright rather than trusting whatever arrives.
    pub expected_digest: Option<&'a str>,
    /// Where that digest is declared, named in a refusal so the operator
    /// knows which entry to correct — e.g. "the `.orbit/plugins.yaml` pin for
    /// 'graph'".
    pub digest_origin: &'a str,
}

/// Fetch or locate `request.source`. Network and filesystem work only; the
/// manifest is read by the caller.
pub fn resolve_plugin_source(
    request: &PluginSourceRequest<'_>,
) -> Result<ResolvedSource, OrbitError> {
    let resolved = resolve_plugin_source_unverified(request)?;
    refuse_plugin_tree_symlinks(&resolved.root)?;
    Ok(resolved)
}

fn resolve_plugin_source_unverified(
    request: &PluginSourceRequest<'_>,
) -> Result<ResolvedSource, OrbitError> {
    let source = request.source;
    // Before anything looks at the shape: a `-`-prefixed value is read as an
    // option by every fetcher this module can reach, and no legitimate
    // source begins with one.
    if source.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{source}' begins with `-`, which a fetcher reads as an option rather \
             than a source"
        )));
    }
    if let Some(spec) = source.strip_prefix("git+") {
        return clone_git_source(spec);
    }
    if let Some(url) = remote_archive_source(source) {
        return fetch_remote_archive(url, request);
    }
    if source.starts_with("https://") {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{source}' is an `https://` URL that does not name an archive; use a \
             `.tar.gz`, `.tgz`, `.tar` or `.zip` URL, or a `git+<url>#<ref>` reference"
        )));
    }
    let path = Path::new(source);
    if path.is_dir() {
        let root = std::fs::canonicalize(path).map_err(|error| {
            OrbitError::InvalidInput(format!("plugin source '{source}': {error}"))
        })?;
        return Ok(ResolvedSource {
            root,
            scratch: None,
            archive_digest: None,
        });
    }
    if path.is_file() {
        return unpack_local_archive(path);
    }
    Err(OrbitError::InvalidInput(format!(
        "plugin source '{source}' is not a directory, an archive, or a `git+<url>#<ref>` \
         reference"
    )))
}

fn clone_git_source(spec: &str) -> Result<ResolvedSource, OrbitError> {
    let (url, reference) = match spec.split_once('#') {
        Some((url, reference)) => (url, Some(reference)),
        None => (spec, None),
    };
    if url.trim().is_empty() {
        return Err(OrbitError::InvalidInput(
            "plugin source 'git+' names no repository URL".to_string(),
        ));
    }
    if !allowed_git_url(url) {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source 'git+{spec}' uses an unsupported Git repository URL; use an `https://`, \
             `ssh://`, or `git@host:path` URL"
        )));
    }
    if reference.is_some_and(|value| value.starts_with('-')) {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source 'git+{spec}' has a Git ref beginning with `-`, which is not allowed"
        )));
    }
    let scratch = new_scratch()?;
    let checkout = scratch.path().join("checkout");
    let checkout_arg = checkout.to_string_lossy().into_owned();

    let mut args = vec!["clone".to_string(), "--depth".to_string(), "1".to_string()];
    if let Some(reference) = reference.filter(|value| !value.trim().is_empty()) {
        args.push("--branch".to_string());
        args.push(reference.to_string());
    }
    args.push("--".to_string());
    args.push(url.to_string());
    args.push(checkout_arg);
    run_git(args, None)?;

    // `git clone` writes the source URL (credentials included, when the URL
    // carried them) into `.git/config`, and `copy_tree` walks this root
    // verbatim into the install path. Drop the clone's VCS metadata here so
    // it never reaches the tree the plugin backend can always read.
    let git_dir = checkout.join(".git");
    if git_dir.exists() {
        std::fs::remove_dir_all(&git_dir)
            .map_err(|error| OrbitError::Io(format!("remove clone metadata: {error}")))?;
    }

    Ok(ResolvedSource {
        root: std::fs::canonicalize(&checkout)
            .map_err(|error| OrbitError::Io(format!("clone target: {error}")))?,
        scratch: Some(scratch),
        archive_digest: None,
    })
}

fn allowed_git_url(url: &str) -> bool {
    if ["https://", "ssh://"].iter().any(|prefix| {
        url.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
    }) {
        return true;
    }
    let Some((host, path)) = url
        .strip_prefix("git@")
        .and_then(|rest| rest.split_once(':'))
    else {
        return false;
    };
    !host.is_empty()
        && !path.is_empty()
        && !host.starts_with('-')
        && !host.chars().any(char::is_whitespace)
}

fn run_git(args: Vec<String>, current_dir: Option<String>) -> Result<(), OrbitError> {
    let mut args_with_protocol_policy = vec![
        "-c".to_string(),
        "protocol.allow=never".to_string(),
        "-c".to_string(),
        "protocol.https.allow=always".to_string(),
        "-c".to_string(),
        "protocol.ssh.allow=always".to_string(),
    ];
    args_with_protocol_policy.extend(args);
    let mut environment = allowlisted_child_env(&[], &[]);
    environment.extend([
        ("GIT_PROTOCOL_FROM_USER".to_string(), "0".to_string()),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
    ]);
    let result = run_process(
        &ExecRequest {
            program: "git".to_string(),
            args: args_with_protocol_policy,
            current_dir,
            timeout_ms: Some(TIMEOUT_LONG_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(environment),
            debug: false,
        },
        &NoSandbox,
    )?;
    if result.success {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "cannot fetch the plugin source: {}",
        result.stderr.trim()
    )))
}

/// Download `url`, check it against the pinned digest, and unpack it.
///
/// The order matters: nothing is unpacked until the whole archive is on disk
/// and hashes to what the pin says, so a tampered archive never reaches the
/// extraction code at all.
fn fetch_remote_archive(
    url: &str,
    request: &PluginSourceRequest<'_>,
) -> Result<ResolvedSource, OrbitError> {
    let Some(declared) = request.expected_digest else {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{url}' is fetched over the network and must be pinned by digest: add \
             a `sha256:` digest to {}. Orbit never installs a downloaded archive on first sight",
            request.digest_origin
        )));
    };
    let expected = parse_archive_digest(declared)
        .map_err(|error| OrbitError::InvalidInput(format!("{}: {error}", request.digest_origin)))?;
    validate_archive_url(url)?;
    let Some(kind) = archive_kind(url_path(url)) else {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{url}' is not a supported archive; use a `.tar.gz`, `.tgz`, `.tar` or \
             `.zip` URL"
        )));
    };

    let scratch = new_scratch()?;
    let download = scratch.path().join("download");
    run_curl(url, &download)?;
    refuse_oversize_archive(&download, url)?;
    let bytes = std::fs::read(&download)
        .map_err(|error| OrbitError::Io(format!("read the fetched archive: {error}")))?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{url}' hashes to sha256:{actual}, but {} pins sha256:{expected}; \
             refusing to install an archive that is not the pinned one",
            request.digest_origin
        )));
    }
    let root = unpack_into(&download, kind, url, scratch.path())?;
    Ok(ResolvedSource {
        root,
        scratch: Some(scratch),
        archive_digest: Some(actual),
    })
}

/// ORB-12812's URL rules, applied before anything spawns: HTTPS only, and no
/// whitespace or control character that would split one argument into two.
fn validate_archive_url(url: &str) -> Result<(), OrbitError> {
    let refuse = |why: &str| {
        Err(OrbitError::InvalidInput(format!(
            "plugin source '{url}' {why}; a fetched plugin archive must be a plain `https://` URL"
        )))
    };
    let Some(rest) = url.strip_prefix("https://") else {
        return refuse("is not an `https://` URL");
    };
    if rest.is_empty() {
        return refuse("names no host");
    }
    if url
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return refuse("contains whitespace or a control character");
    }
    Ok(())
}

/// Download `url` to `destination`.
///
/// The transport rules are curl's own so they hold for every hop rather than
/// only the first: `--proto =https` admits no other scheme, `--proto-redir
/// =https` refuses a redirect that changes scheme, and `--disable` stops a
/// host `.curlrc` from re-enabling either.
fn run_curl(url: &str, destination: &Path) -> Result<(), OrbitError> {
    let args = vec![
        "--disable".to_string(),
        "--fail".to_string(),
        "--silent".to_string(),
        "--show-error".to_string(),
        "--location".to_string(),
        "--proto".to_string(),
        "=https".to_string(),
        "--proto-redir".to_string(),
        "=https".to_string(),
        "--max-redirs".to_string(),
        MAX_REDIRECTS.to_string(),
        "--max-filesize".to_string(),
        MAX_ARCHIVE_BYTES.to_string(),
        "--output".to_string(),
        destination.to_string_lossy().into_owned(),
        "--".to_string(),
        url.to_string(),
    ];
    // A custom trust store belongs to the operator, and is the only thing
    // beyond the baseline environment this fetch is allowed to read.
    let environment = allowlisted_child_env(&[], &["SSL_CERT_FILE", "SSL_CERT_DIR"]);
    let result = run_process(
        &ExecRequest {
            program: "curl".to_string(),
            args,
            current_dir: None,
            timeout_ms: Some(TIMEOUT_LONG_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(environment),
            debug: false,
        },
        &NoSandbox,
    )
    .map_err(|error| {
        OrbitError::Execution(format!(
            "cannot fetch the plugin archive '{url}': {error}; fetching an `https://` plugin \
             source needs `curl` on PATH"
        ))
    })?;
    if result.success {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "cannot fetch the plugin archive '{url}': {}",
        result.stderr.trim()
    )))
}

fn refuse_oversize_archive(path: &Path, source_name: &str) -> Result<(), OrbitError> {
    let size = std::fs::metadata(path)
        .map_err(|error| OrbitError::Io(format!("read {source_name}: {error}")))?
        .len();
    if size > MAX_ARCHIVE_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "plugin archive '{source_name}' is {size} bytes, past the {MAX_ARCHIVE_BYTES}-byte \
             limit for a plugin source"
        )));
    }
    Ok(())
}

/// Archive containers a plugin source may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveKind {
    TarGz,
    Tar,
    Zip,
}

/// The part of a URL before any query string or fragment, which is the only
/// part whose suffix names the archive's container.
fn url_path(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

fn archive_kind(name: &str) -> Option<ArchiveKind> {
    let lowered = name.to_ascii_lowercase();
    if lowered.ends_with(".tar.gz") || lowered.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if lowered.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else if lowered.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

fn new_scratch() -> Result<tempfile::TempDir, OrbitError> {
    tempfile::Builder::new()
        .prefix("orbit-plugin-src-")
        .tempdir()
        .map_err(|error| OrbitError::Io(format!("create plugin scratch dir: {error}")))
}

fn unpack_local_archive(path: &Path) -> Result<ResolvedSource, OrbitError> {
    let name = path.to_string_lossy().into_owned();
    let Some(kind) = archive_kind(&name) else {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{name}' is not a supported archive; use a `.tar.gz`, `.tgz`, `.tar` \
             or `.zip` file, a directory, or `git+<url>#<ref>`"
        )));
    };
    let scratch = new_scratch()?;
    let root = unpack_into(path, kind, &name, scratch.path())?;
    Ok(ResolvedSource {
        root,
        scratch: Some(scratch),
        archive_digest: None,
    })
}

/// Unpack `path` below `scratch` and return the directory holding the
/// manifest.
fn unpack_into(
    path: &Path,
    kind: ArchiveKind,
    source_name: &str,
    scratch: &Path,
) -> Result<PathBuf, OrbitError> {
    refuse_oversize_archive(path, source_name)?;
    let unpacked = scratch.join("archive");
    std::fs::create_dir_all(&unpacked)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", unpacked.display())))?;
    let file = std::fs::File::open(path)
        .map_err(|error| OrbitError::Io(format!("open {source_name}: {error}")))?;
    let limits = ArchiveLimits::DEFAULT;
    match kind {
        ArchiveKind::TarGz => unpack_tar(
            flate2::read::GzDecoder::new(file),
            &unpacked,
            source_name,
            limits,
        )?,
        ArchiveKind::Tar => unpack_tar(file, &unpacked, source_name, limits)?,
        ArchiveKind::Zip => unpack_zip(file, &unpacked, source_name, limits)?,
    }
    plugin_root_within(&unpacked)
}

/// Unpack one tar, refusing every member that would write outside `dest` or
/// recreate a symbolic link, and stopping at the size and entry bounds.
///
/// The path check is explicit rather than delegated: `tar::Entry::unpack_in`
/// *skips* a member that escapes `dest` and reports success, which would
/// install a silently incomplete tree instead of failing the install.
/// `Archive::unpack` would recreate a symlink member; `copy_tree` would then
/// follow it and materialise the target's bytes in the install root.
///
/// A hardlink entry is not refused the same way: `unpack_in` resolves a
/// hardlink's target against `dest` and validates it stays inside that
/// directory before calling `fs::hard_link`, so unlike a symlink there is no
/// separate escape for this code to guard against.
pub(super) fn unpack_tar<R: Read>(
    reader: R,
    dest: &Path,
    source_name: &str,
    limits: ArchiveLimits,
) -> Result<(), OrbitError> {
    let mut archive = tar::Archive::new(BoundedReader::new(
        reader,
        limits.unpacked_bytes,
        source_name,
    ));
    let entries = archive
        .entries()
        .map_err(|error| unpack_failure(source_name, &error))?;
    let mut count = 0usize;
    for entry in entries {
        let mut entry = entry.map_err(|error| unpack_failure(source_name, &error))?;
        count += 1;
        if count > limits.entries {
            return Err(too_many_entries(source_name, limits.entries));
        }
        let path = entry
            .path()
            .map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?
            .into_owned();
        if entry.header().entry_type().is_symlink() {
            let target = entry.link_name().ok().flatten();
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                &path,
                target.as_deref(),
            )));
        }
        refuse_escaping_member(&path, source_name)?;
        entry
            .unpack_in(dest)
            .map_err(|error| unpack_failure(source_name, &error))?;
    }
    Ok(())
}

/// Report an unpack failure with its whole cause chain.
///
/// `tar` wraps a reader error in "failed to unpack `<path>`", which would
/// otherwise bury the bound this module is enforcing and leave the operator
/// with a message that does not say what went wrong.
fn unpack_failure(source_name: &str, error: &std::io::Error) -> OrbitError {
    let mut causes = vec![error.to_string()];
    let mut cause = std::error::Error::source(error);
    while let Some(inner) = cause {
        causes.push(inner.to_string());
        cause = inner.source();
    }
    OrbitError::Io(format!("unpack {source_name}: {}", causes.join(": ")))
}

/// Unpack one zip under the same rules as [`unpack_tar`].
///
/// A zip member name is a bare string with no safety of its own, so every
/// refusal here is this function's: `..`, an absolute path, a Windows
/// separator that a zip writer may have produced, and a member whose Unix
/// mode marks it a symbolic link.
pub(super) fn unpack_zip(
    file: std::fs::File,
    dest: &Path,
    source_name: &str,
    limits: ArchiveLimits,
) -> Result<(), OrbitError> {
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file)).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "plugin archive '{source_name}' is not a readable zip: {error}"
        ))
    })?;
    if archive.len() > limits.entries {
        return Err(too_many_entries(source_name, limits.entries));
    }
    let mut budget = limits.unpacked_bytes;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?;
        let name = entry.name().to_string();
        let member = PathBuf::from(&name);
        if name.contains('\\') {
            return Err(OrbitError::InvalidInput(format!(
                "plugin archive '{source_name}' contains the entry '{name}', which uses `\\` as a \
                 path separator; refusing to unpack it"
            )));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                &member, None,
            )));
        }
        refuse_escaping_member(&member, source_name)?;
        let target = dest.join(&member);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", target.display())))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| OrbitError::Io(format!("create {}: {error}", parent.display())))?;
        }
        let mut out = std::fs::File::create(&target)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", target.display())))?;
        // The header's own size is attacker-controlled, so the copy is what
        // enforces the bound, not the declared length.
        let written = std::io::copy(
            &mut BoundedReader::new(&mut entry, budget, source_name),
            &mut out,
        )
        .map_err(|error| unpack_failure(source_name, &error))?;
        budget = budget.saturating_sub(written);
        apply_zip_mode(&target, entry.unix_mode())?;
    }
    Ok(())
}

#[cfg(unix)]
fn apply_zip_mode(target: &Path, mode: Option<u32>) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;
    let Some(mode) = mode else {
        return Ok(());
    };
    // Only the ordinary permission bits: setuid, setgid and sticky are never
    // carried into a plugin tree from an archive.
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode & 0o777))
        .map_err(|error| OrbitError::Io(format!("set mode on {}: {error}", target.display())))
}

#[cfg(not(unix))]
fn apply_zip_mode(_target: &Path, _mode: Option<u32>) -> Result<(), OrbitError> {
    Ok(())
}

fn too_many_entries(source_name: &str, limit: usize) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "plugin archive '{source_name}' has more than {limit} entries; refusing to unpack it"
    ))
}

/// Refuse a member whose path would write outside the unpack root.
fn refuse_escaping_member(path: &Path, source_name: &str) -> Result<(), OrbitError> {
    let refuse = |why: &str| {
        Err(OrbitError::InvalidInput(format!(
            "plugin archive '{source_name}' contains the entry '{}', which {why}; refusing to \
             unpack it",
            path.display()
        )))
    };
    if path.components().next().is_none() {
        return refuse("has an empty path");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return refuse("traverses out of the archive root with `..`");
            }
            Component::RootDir | Component::Prefix(_) => {
                return refuse("is an absolute path");
            }
        }
    }
    Ok(())
}

/// A reader that fails the unpack once the stream passes `limit` bytes.
///
/// One byte of headroom is kept so reaching exactly `limit` is still a
/// successful read: the error fires only on the byte that would exceed it.
struct BoundedReader<R> {
    inner: R,
    remaining: u64,
    limit: u64,
    source_name: String,
}

impl<R: Read> BoundedReader<R> {
    fn new(inner: R, limit: u64, source_name: &str) -> Self {
        Self {
            inner,
            remaining: limit.saturating_add(1),
            limit,
            source_name: source_name.to_string(),
        }
    }
}

impl<R: Read> Read for BoundedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let capacity = usize::try_from(self.remaining)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        if capacity == 0 {
            return Err(std::io::Error::other(format!(
                "plugin archive '{}' unpacks to more than {} bytes",
                self.source_name, self.limit
            )));
        }
        let read = self.inner.read(&mut buf[..capacity])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// An archive may hold the manifest at its top level or inside one wrapper
/// directory, which is what `git archive` and release tarballs produce.
fn plugin_root_within(unpacked: &Path) -> Result<PathBuf, OrbitError> {
    if unpacked
        .join(orbit_types::plugin::MANIFEST_FILE_NAME)
        .is_file()
    {
        return Ok(unpacked.to_path_buf());
    }
    let mut entries = std::fs::read_dir(unpacked)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", unpacked.display())))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    entries.sort();
    if let [single] = entries.as_slice()
        && single
            .join(orbit_types::plugin::MANIFEST_FILE_NAME)
            .is_file()
    {
        return Ok(single.clone());
    }
    Err(OrbitError::InvalidInput(format!(
        "the archive does not contain a {} at its root or in a single top-level directory",
        orbit_types::plugin::MANIFEST_FILE_NAME
    )))
}
