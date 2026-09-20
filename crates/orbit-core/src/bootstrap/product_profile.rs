//! Application-owned bootstrap identity. This is not a workspace setting.
//!
//! Only Orbit is available in production. The alternate profile is deliberately
//! test-only until execution admission and child-process re-entry carry product
//! identity. A directory marker protects supported composition/bootstrap paths;
//! it is not an authorization boundary against raw filesystem/store access.

use std::fs;
use std::path::Path;

use orbit_common::OrbitError;

pub(crate) const PRODUCT_MARKER: &str = ".orbit-product";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProductProfile {
    Orbit,
    #[cfg(test)]
    ResearchFixture,
}

impl ProductProfile {
    pub(crate) fn identity(self) -> &'static str {
        match self {
            Self::Orbit => "orbit:v1\n",
            #[cfg(test)]
            Self::ResearchFixture => "orbit-research-fixture:v1\n",
        }
    }

    /// Read every supplied root before any caller starts generation admission,
    /// layout upgrades, forced deletion, config seeding, or reconciliation.
    pub(crate) fn validate_roots(self, roots: &[&Path]) -> Result<(), OrbitError> {
        for root in roots {
            self.validate_root(root)?;
        }
        Ok(())
    }

    fn validate_root(self, root: &Path) -> Result<(), OrbitError> {
        if self != Self::Orbit
            && (!root.is_absolute()
                || root
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir)))
        {
            return Err(OrbitError::InvalidInput(
                "alternate product roots must be explicit absolute paths without parent traversal"
                    .into(),
            ));
        }
        // Check physical ancestry as well as the selected directory: a fresh
        // child of another product root is not an independent storage root.
        for ancestor in root.ancestors() {
            match fs::canonicalize(ancestor) {
                Ok(physical) => {
                    for parent in physical.ancestors() {
                        self.validate_marker(parent)?;
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(OrbitError::Io(error.to_string())),
            }
        }
        // Existing Orbit roots predate product identity. Only Orbit may
        // adopt them. An alternate product must start empty or already
        // carry its own marker; it must never guess from filenames.
        if !self.validate_marker(root)? && self != Self::Orbit && root.exists() {
            let mut entries =
                fs::read_dir(root).map_err(|error| OrbitError::Io(error.to_string()))?;
            if entries
                .next()
                .transpose()
                .map_err(|error| OrbitError::Io(error.to_string()))?
                .is_some()
            {
                return Err(OrbitError::InvalidInput(format!(
                    "refusing unmarked nonempty product root: {}",
                    root.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_marker(self, root: &Path) -> Result<bool, OrbitError> {
        let marker = root.join(PRODUCT_MARKER);
        match fs::symlink_metadata(&marker) {
            Ok(metadata) => {
                if !metadata.file_type().is_file() {
                    return Err(OrbitError::InvalidInput(format!(
                        "product marker must be a regular file: {}",
                        marker.display()
                    )));
                }
                let identity = fs::read_to_string(&marker)
                    .map_err(|error| OrbitError::Io(error.to_string()))?;
                if identity != self.identity() {
                    return Err(OrbitError::InvalidInput(format!(
                        "product ownership mismatch at {}: expected {}",
                        root.display(),
                        self.identity().trim()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(OrbitError::Io(error.to_string())),
        }
        Ok(true)
    }

    /// Claim an explicitly initialized root, without replacing an existing
    /// marker. Composition-only opens validate without claiming; bootstrap
    /// initialization records ownership when the root is writable.
    pub(crate) fn claim_root(self, root: &Path) -> Result<(), OrbitError> {
        use std::io::Write;
        self.validate_root(root)?;
        orbit_common::fs::io::create_private_dir_all(root)
            .map_err(|error| OrbitError::Io(error.to_string()))?;
        let marker = root.join(PRODUCT_MARKER);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(mut file) => file
                .write_all(self.identity().as_bytes())
                .map_err(|error| OrbitError::Io(error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.validate_root(root)
            }
            Err(error) => Err(OrbitError::Io(error.to_string())),
        }
    }
}

/// Minimal alternative catalog: a shared safety policy, no engineering assets.
/// This function is intentionally absent from production builds.
#[cfg(test)]
pub(crate) fn initialize_research_catalog(
    roots: &crate::runtime::OrbitRuntimeRoots,
) -> Result<(), OrbitError> {
    let profile = ProductProfile::ResearchFixture;
    let selected = [
        roots.global_root.as_path(),
        roots.shared_root.as_path(),
        roots.local_root.as_path(),
    ];
    profile.validate_roots(&selected)?;
    for root in selected {
        profile.claim_root(root)?;
    }
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        &roots.global_root,
        &roots.shared_root,
    ))?;
    let policy_store = orbit_store::compose::global_policy_def_store(config.persistence.policy_dir);
    crate::bootstrap::policy::seed_default_policies(policy_store.as_ref(), false)?;
    Ok(())
}
