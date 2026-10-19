use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::super::DispatchError;
use super::super::fingerprint::git_stdout;
use super::super::rebase_repairs::auto_merge_tree;

/// In-memory host evidence, never loaded from provider output or scratch.
/// Hold directory handles so inode reuse cannot authenticate a replacement.
pub(super) struct RecoveryMetadata {
    pointer: Vec<u8>,
    pointer_handle: fs::File,
    directories: Vec<(PathBuf, fs::File)>,
    rebase_root: PathBuf,
    rebase_files: BTreeMap<PathBuf, Vec<u8>>,
    auto_merge_tree: Option<String>,
}

impl RecoveryMetadata {
    pub(super) fn capture(root: &Path, rebase_root: &Path) -> Result<Self, DispatchError> {
        let mut directories = Vec::new();
        for flag in ["--absolute-git-dir", "--git-common-dir"] {
            let raw = git_stdout(root, &["rev-parse", "--path-format=absolute", flag])?;
            let path = PathBuf::from(raw).canonicalize().map_err(metadata_error)?;
            let handle = fs::File::open(&path).map_err(metadata_error)?;
            directories.push((path, handle));
        }
        Ok(Self {
            pointer: fs::read(root.join(".git")).map_err(metadata_error)?,
            pointer_handle: fs::File::open(root.join(".git")).map_err(metadata_error)?,
            directories,
            rebase_root: rebase_root.to_path_buf(),
            rebase_files: recovery_metadata_files(rebase_root)?,
            auto_merge_tree: auto_merge_tree(root)?,
        })
    }

    pub(super) fn verify(&self, root: &Path) -> Result<(), DispatchError> {
        let invalid = || {
            DispatchError::CliInvocationPermanent(
                "conflict recovery refused changed Git metadata identity or recovery instructions"
                    .to_string(),
            )
        };
        if !same_metadata_entry(&root.join(".git"), &self.pointer_handle)?
            || fs::read(root.join(".git")).map_err(metadata_error)? != self.pointer
        {
            return Err(invalid());
        }
        for ((expected, handle), flag) in self
            .directories
            .iter()
            .zip(["--absolute-git-dir", "--git-common-dir"])
        {
            let raw = git_stdout(root, &["rev-parse", "--path-format=absolute", flag])?;
            let path = PathBuf::from(raw).canonicalize().map_err(metadata_error)?;
            if &path != expected {
                return Err(invalid());
            }
            if !same_metadata_entry(&path, handle)? {
                return Err(invalid());
            }
        }
        if recovery_metadata_files(&self.rebase_root)? != self.rebase_files
            || auto_merge_tree(root)? != self.auto_merge_tree
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn same_metadata_entry(path: &Path, handle: &fs::File) -> Result<bool, DispatchError> {
    let after = fs::symlink_metadata(path).map_err(metadata_error)?;
    if after.file_type().is_symlink() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let before = handle.metadata().map_err(metadata_error)?;
        Ok((before.dev(), before.ino()) == (after.dev(), after.ino()))
    }
    #[cfg(not(unix))]
    {
        // Linux/macOS enforce inode identity; other platforms retain the
        // pointer, canonical-path and instruction-content checks above.
        let _ = handle;
        Ok(true)
    }
}

fn metadata_error(error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("inspect host recovery metadata: {error}"))
}

fn recovery_metadata_files(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>, DispatchError> {
    if !fs::symlink_metadata(root).map_err(metadata_error)?.is_dir() {
        return Err(DispatchError::CliInvocationPermanent(
            "host recovery metadata root is not a directory".to_string(),
        ));
    }
    let mut files = BTreeMap::new();
    for entry in fs::read_dir(root).map_err(metadata_error)? {
        let entry = entry.map_err(metadata_error)?;
        let kind = entry.file_type().map_err(metadata_error)?;
        if kind.is_dir() {
            files.extend(recovery_metadata_files(&entry.path())?);
        } else if kind.is_file() {
            let content = fs::read(entry.path()).map_err(metadata_error)?;
            files.insert(entry.path(), Sha256::digest(content).to_vec());
        } else {
            return Err(DispatchError::CliInvocationPermanent(
                "host recovery metadata contains a symlink or special file".to_string(),
            ));
        }
    }
    Ok(files)
}
