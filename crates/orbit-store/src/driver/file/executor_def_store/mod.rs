//! Directory layout of executor definitions: one `<name>.yaml` per
//! definition directly under one root.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::resource::validate_resource_name;

/// The directory that holds executor definitions.
#[derive(Debug, Clone)]
pub struct ExecutorDefDir {
    root: PathBuf,
}

impl ExecutorDefDir {
    /// Executor definitions directly under `root`.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The file that holds the definition named `name`, after checking the
    /// name is a safe resource name.
    pub(crate) fn path(&self, name: &str) -> Result<PathBuf, OrbitError> {
        validate_resource_name(name)?;
        Ok(self.root.join(format!("{name}.yaml")))
    }

    /// Create the directory, private to the user, before a write.
    pub(crate) fn ensure(&self) -> Result<(), OrbitError> {
        orbit_common::fs::io::create_private_dir_all(&self.root)?;
        Ok(())
    }

    /// Every `*.yaml` file under the root, in directory order. An absent
    /// root has none.
    pub(crate) fn yaml_files(&self) -> Result<Vec<PathBuf>, OrbitError> {
        yaml_files(&self.root)
    }
}

fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).map_err(|e| OrbitError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            files.push(path);
        }
    }
    Ok(files)
}
