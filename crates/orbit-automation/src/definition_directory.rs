use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) enum DefinitionDirectoryError {
    Missing,
    Invalid(String),
}

pub(crate) fn canonical_definition_root(root: &Path) -> Result<PathBuf, DefinitionDirectoryError> {
    fs::canonicalize(root).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            DefinitionDirectoryError::Missing
        } else {
            DefinitionDirectoryError::Invalid(format!(
                "failed to resolve Orbit directory {}: {error}",
                root.display()
            ))
        }
    })
}

pub(crate) fn resolve_definition_child(
    parent: &Path,
    name: &str,
    label: &str,
) -> Result<PathBuf, DefinitionDirectoryError> {
    let expected = parent.join(name);
    if !expected.starts_with(parent) {
        return Err(DefinitionDirectoryError::Invalid(format!(
            "{label} directory escapes {}",
            parent.display()
        )));
    }
    let invalid = || {
        DefinitionDirectoryError::Invalid(format!(
            "{label} directory must be a regular directory directly under {}",
            parent.display()
        ))
    };
    if fs::symlink_metadata(&expected).is_ok_and(|metadata| !metadata.is_dir()) {
        return Err(invalid());
    }
    let canonical = fs::canonicalize(&expected).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            DefinitionDirectoryError::Missing
        } else {
            DefinitionDirectoryError::Invalid(format!(
                "failed to resolve {label} directory {}: {error}",
                expected.display()
            ))
        }
    })?;
    if canonical != expected || !canonical.is_dir() {
        return Err(invalid());
    }
    Ok(canonical)
}

pub(crate) enum DefinitionEntryError {
    Read(io::Error),
    Inspect(PathBuf, io::Error),
}

pub(crate) fn list_definition_yaml(
    dir: &Path,
) -> io::Result<(Vec<PathBuf>, Vec<DefinitionEntryError>)> {
    let mut paths = Vec::new();
    let mut errors = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(DefinitionEntryError::Read(error));
                continue;
            }
        };
        let path = dir.join(entry.file_name());
        if !path.starts_with(dir) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(DefinitionEntryError::Inspect(path, error));
                continue;
            }
        };
        if metadata.file_type().is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml")
                })
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok((paths, errors))
}
