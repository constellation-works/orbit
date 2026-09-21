//! Resolve an `orbit plugin add` source to a local directory.
//!
//! Three forms (§3): a local directory, `git+<url>[#<ref>]`, and a
//! `.tar.gz`/`.tgz`/`.tar` archive. Fetching runs here rather than in Core
//! because this is the crate that owns spawning a process.

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

use super::loader::{plugin_symlink_refusal, refuse_plugin_tree_symlinks};
use crate::TIMEOUT_LONG_MS;

/// Where a resolved source's tree lives, and what the source was.
#[derive(Debug)]
pub struct ResolvedSource {
    /// Directory holding `plugin.yaml`.
    pub root: PathBuf,
    /// Set when the tree was fetched into scratch; dropping it removes the
    /// scratch directory, so the caller holds it until the copy is done.
    pub scratch: Option<tempfile::TempDir>,
}

/// Fetch or locate `source`. Network and filesystem work only; the manifest
/// is read by the caller.
pub fn resolve_plugin_source(source: &str) -> Result<ResolvedSource, OrbitError> {
    let resolved = resolve_plugin_source_unverified(source)?;
    refuse_plugin_tree_symlinks(&resolved.root)?;
    Ok(resolved)
}

fn resolve_plugin_source_unverified(source: &str) -> Result<ResolvedSource, OrbitError> {
    if let Some(spec) = source.strip_prefix("git+") {
        return clone_git_source(spec);
    }
    let path = Path::new(source);
    if path.is_dir() {
        let root = std::fs::canonicalize(path).map_err(|error| {
            OrbitError::InvalidInput(format!("plugin source '{source}': {error}"))
        })?;
        return Ok(ResolvedSource {
            root,
            scratch: None,
        });
    }
    if path.is_file() {
        return unpack_archive(path);
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
    let scratch = tempfile::Builder::new()
        .prefix("orbit-plugin-src-")
        .tempdir()
        .map_err(|error| OrbitError::Io(format!("create plugin scratch dir: {error}")))?;
    let checkout = scratch.path().join("checkout");
    let checkout_arg = checkout.to_string_lossy().into_owned();

    let mut args = vec!["clone".to_string(), "--depth".to_string(), "1".to_string()];
    if let Some(reference) = reference.filter(|value| !value.trim().is_empty()) {
        args.push("--branch".to_string());
        args.push(reference.to_string());
    }
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
    })
}

fn run_git(args: Vec<String>, current_dir: Option<String>) -> Result<(), OrbitError> {
    let result = run_process(
        &ExecRequest {
            program: "git".to_string(),
            args,
            current_dir,
            timeout_ms: Some(TIMEOUT_LONG_MS),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(allowlisted_child_env(&[], &[])),
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

fn unpack_archive(path: &Path) -> Result<ResolvedSource, OrbitError> {
    let name = path.to_string_lossy();
    let gzipped = name.ends_with(".tar.gz") || name.ends_with(".tgz");
    if !gzipped && !name.ends_with(".tar") {
        return Err(OrbitError::InvalidInput(format!(
            "plugin source '{name}' is not a supported archive; use a `.tar.gz`, `.tgz` or \
             `.tar` file, a directory, or `git+<url>#<ref>`"
        )));
    }
    let scratch = tempfile::Builder::new()
        .prefix("orbit-plugin-src-")
        .tempdir()
        .map_err(|error| OrbitError::Io(format!("create plugin scratch dir: {error}")))?;
    let file = std::fs::File::open(path)
        .map_err(|error| OrbitError::Io(format!("open {name}: {error}")))?;
    let unpacked = scratch.path().join("archive");
    std::fs::create_dir_all(&unpacked)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", unpacked.display())))?;
    if gzipped {
        unpack_tar(
            tar::Archive::new(flate2::read::GzDecoder::new(file)),
            &unpacked,
            &name,
        )?;
    } else {
        unpack_tar(tar::Archive::new(file), &unpacked, &name)?;
    }
    let root = plugin_root_within(&unpacked)?;
    Ok(ResolvedSource {
        root,
        scratch: Some(scratch),
    })
}

/// Unpack one archive, refusing symlink members before they hit the disk.
/// `Archive::unpack` would recreate those links; `copy_tree` would then
/// follow them and materialise the target's bytes in the install root.
///
/// A hardlink entry is not refused the same way: `tar::Entry::unpack_in`
/// (called below) resolves a hardlink's target against `dest` and validates
/// it stays inside that directory before calling `fs::hard_link`, so unlike
/// a symlink there is no separate escape for this code to guard against.
fn unpack_tar<R: Read>(
    mut archive: tar::Archive<R>,
    dest: &Path,
    source_name: &str,
) -> Result<(), OrbitError> {
    let entries = archive
        .entries()
        .map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?;
        if entry.header().entry_type().is_symlink() {
            let name = entry
                .path()
                .map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?;
            let target = entry.link_name().ok().flatten();
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                name.as_ref(),
                target.as_deref(),
            )));
        }
        entry
            .unpack_in(dest)
            .map_err(|error| OrbitError::Io(format!("unpack {source_name}: {error}")))?;
    }
    Ok(())
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
