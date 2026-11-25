//! Directory layout of policy definitions: one `<name>.yaml` (or an existing
//! `<name>.yml`) per definition under a root, optionally layered over a
//! global root.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_types::resource::validate_resource_name;

/// One directory of policy definitions.
#[derive(Debug, Clone)]
pub(crate) struct PolicyDefDir {
    root: PathBuf,
}

impl PolicyDefDir {
    /// The file that holds the definition named `name`, after checking the
    /// name is a safe resource name.
    pub(crate) fn path(&self, name: &str) -> Result<PathBuf, OrbitError> {
        validate_resource_name(name)?;
        let yaml = self.root.join(format!("{name}.yaml"));
        let yml = self.root.join(format!("{name}.yml"));
        // Keep the established .yaml preference if both variants exist, but
        // update an existing .yml in place instead of creating a second file.
        Ok(if yaml.exists() || !yml.exists() {
            yaml
        } else {
            yml
        })
    }

    /// Create the directory, private to the user, before a write.
    pub(crate) fn ensure_layout(&self) -> Result<(), OrbitError> {
        orbit_common::fs::io::create_private_dir_all(&self.root)?;
        Ok(())
    }

    /// The names of every `*.yaml` or `*.yml` file, sorted and deduplicated.
    /// An absent directory has none.
    pub(crate) fn names(&self) -> Result<BTreeSet<String>, OrbitError> {
        let mut names = BTreeSet::new();
        if !self.root.exists() {
            return Ok(names);
        }
        let entries = fs::read_dir(&self.root).map_err(|e| OrbitError::Io(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| OrbitError::Io(e.to_string()))?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext == "yaml" || ext == "yml")
            {
                let name = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "invalid policy file name: {}",
                            path.display()
                        ))
                    })?;
                names.insert(name.to_owned());
            }
        }
        Ok(names)
    }
}

/// Policy definitions in a primary directory, optionally layered over a
/// global one. Writes go to the primary directory.
#[derive(Debug, Clone)]
pub struct PolicyDefDirs {
    pub(crate) primary: PolicyDefDir,
    pub(crate) global: Option<PolicyDefDir>,
}

impl PolicyDefDirs {
    /// Policy definitions in `root` alone.
    pub fn single(root: PathBuf) -> Self {
        Self {
            primary: PolicyDefDir { root },
            global: None,
        }
    }

    /// Policy definitions in `workspace`, layered over those in `global`.
    pub(crate) fn layered(workspace: PolicyDefDir, global: PolicyDefDir) -> Self {
        Self {
            primary: workspace,
            global: Some(global),
        }
    }
}
