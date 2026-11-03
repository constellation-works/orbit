//! Declared file inputs of a script step (script steps §4 Files in).
//!
//! Each `files.inputs` entry renders its `from` against the step input and
//! resolves physically, symlinks followed, to a regular file or directory
//! inside the cwd tree or a granted read root. A source that is missing,
//! lies elsewhere, overlaps a masked tree or a credential path, or holds an
//! entry that is neither a file, a directory nor a symlink refuses the step
//! before anything is spawned, naming the input. Admitted inputs are bound
//! read-only at `in/<as>` and recorded with their source and SHA-256: a
//! file's digest is of its bytes, a directory's is the tree digest of
//! [`SCRIPT_INPUT_TREE_DOMAIN`] over every entry's relative path, kind and
//! content. The digest is taken at admission; the bind is made at spawn.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_engine::activity_job::render_script_args;
use orbit_exec::{ScriptSandboxInput, script_input_conflict};
use orbit_types::workflow::activity_job::{
    SCRIPT_INPUT_MAX_TREE_ENTRIES, SCRIPT_INPUT_TREE_DOMAIN, ScriptFileInput, ScriptInputFile,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// What a script may read its inputs from, and what it must never see.
pub(super) struct ScriptInputSources<'a> {
    /// The physical cwd tree.
    pub(super) cwd: &'a Path,
    pub(super) read_roots: &'a [PathBuf],
    pub(super) masked: &'a [PathBuf],
    pub(super) home: Option<&'a OsStr>,
}

/// Resolve, check and digest every declared input, in manifest order. The
/// error names the refused input.
pub(super) fn admit_script_inputs(
    inputs: &[ScriptFileInput],
    step_input: &Value,
    sources: &ScriptInputSources<'_>,
) -> Result<Vec<(ScriptSandboxInput, ScriptInputFile)>, String> {
    let read_roots: Vec<PathBuf> = sources
        .read_roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let mut admitted = Vec::with_capacity(inputs.len());
    for input in inputs {
        let refused = |detail: String| format!("script file input `{}` {detail}", input.as_);
        let from = render_script_args(std::slice::from_ref(&input.from), step_input)
            .map_err(|error| refused(format!("has no usable `from`: {error}")))?
            .remove(0);
        if from.trim().is_empty() {
            return Err(refused("renders an empty `from`".to_string()));
        }
        let candidate = sources.cwd.join(&from);
        let source = candidate.canonicalize().map_err(|error| {
            refused(format!(
                "is missing: {} does not resolve: {error}",
                candidate.display()
            ))
        })?;
        if !source.starts_with(sources.cwd)
            && !read_roots.iter().any(|root| source.starts_with(root))
        {
            return Err(refused(format!(
                "resolves to {}, outside the cwd tree {} and the granted read roots",
                source.display(),
                sources.cwd.display()
            )));
        }
        if let Some(protected) =
            script_input_conflict(&source, sources.cwd, sources.masked, sources.home)
        {
            return Err(refused(format!(
                "resolves to {}, which overlaps protected state at {}",
                source.display(),
                protected.display()
            )));
        }
        let metadata = std::fs::metadata(&source)
            .map_err(|error| refused(format!("cannot be read: {error}")))?;
        let (kind, bytes, sha256) = if metadata.is_file() {
            let (bytes, sha256) =
                hash_file(&source).map_err(|error| refused(format!("cannot be read: {error}")))?;
            ("file", bytes, sha256)
        } else if metadata.is_dir() {
            let (bytes, sha256) = hash_tree(&source).map_err(refused)?;
            ("directory", bytes, sha256)
        } else {
            return Err(refused(format!(
                "resolves to {}, which is not a regular file or directory",
                source.display()
            )));
        };
        admitted.push((
            ScriptSandboxInput {
                source: source.clone(),
                name: input.as_.clone(),
            },
            ScriptInputFile {
                name: input.as_.clone(),
                source,
                kind: kind.to_string(),
                bytes,
                sha256,
            },
        ));
    }
    Ok(admitted)
}

/// The tree digest of a directory: every entry beneath it, depth first in
/// byte order of names, without following symlinks. Returns the regular
/// files' total size with it.
fn hash_tree(root: &Path) -> Result<(u64, String), String> {
    let mut hasher = Sha256::new();
    hasher.update(SCRIPT_INPUT_TREE_DOMAIN.as_bytes());
    let mut total: u64 = 0;
    let mut entries: usize = 0;
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let dir = root.join(&relative);
        let mut children: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|error| format!("cannot list {}: {error}", dir.display()))?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()
            .map_err(|error| format!("cannot list {}: {error}", dir.display()))?;
        children.sort();
        // Reverse so the stack pops subdirectories in name order.
        let mut subdirectories = Vec::new();
        for name in children {
            entries += 1;
            if entries > SCRIPT_INPUT_MAX_TREE_ENTRIES {
                return Err(format!(
                    "holds more than {SCRIPT_INPUT_MAX_TREE_ENTRIES} entries"
                ));
            }
            let child = relative.join(&name);
            let path = root.join(&child);
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let file_type = metadata.file_type();
            hasher.update(b"\n");
            if file_type.is_dir() {
                hasher.update(b"d\0");
                hasher.update(child.as_os_str().as_encoded_bytes());
                subdirectories.push(child);
            } else if file_type.is_file() {
                let (bytes, sha256) = hash_file(&path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
                total = total.saturating_add(bytes);
                hasher.update(b"f\0");
                hasher.update(child.as_os_str().as_encoded_bytes());
                hasher.update(b"\0");
                hasher.update(bytes.to_string().as_bytes());
                hasher.update(b"\0");
                hasher.update(sha256.as_bytes());
            } else if file_type.is_symlink() {
                let target = std::fs::read_link(&path)
                    .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
                hasher.update(b"l\0");
                hasher.update(child.as_os_str().as_encoded_bytes());
                hasher.update(b"\0");
                hasher.update(target.as_os_str().as_encoded_bytes());
            } else {
                return Err(format!(
                    "holds {}, which is not a file, directory or symlink",
                    path.display()
                ));
            }
        }
        pending.extend(subdirectories.into_iter().rev());
    }
    Ok((total, orbit_types::identity::lower_hex(hasher.finalize())))
}

fn hash_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = open_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((total, orbit_types::identity::lower_hex(hasher.finalize())))
}

/// Open without following a final symlink and without blocking on a FIFO
/// swapped in after the listing.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}
