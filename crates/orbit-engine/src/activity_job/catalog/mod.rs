//! v2 activity catalog + `target: activity:<name>` resolution (Phase 4).
//!
//! A catalog is a name → `ActivityV2` map built from one or more directory
//! trees of v2 YAML assets. [`resolve_job_target_refs`] walks a [`JobV2`]
//! DAG and rewrites every [`JobV2StepBody::TargetRef`] into
//! [`JobV2StepBody::Target`] by looking up the named activity in the
//! catalog. Resolution runs after `resolve_job_backends`
//! (so the `Auto` → concrete rewrite also applies to the newly-inlined
//! specs) and before `validate_job_loop_session_backends`.
//!
//! Scope resolution (§9 `MergeByKey`) remains caller policy: orbit-core
//! adapters supply precedence-ordered directory lists and choose whether a
//! lower layer is eligible. This module owns the shared recursive walk,
//! canonical directory deduplication, first-wins layering, and duplicate-name
//! detection for the typed activity and job catalogs.

mod activity;
mod error;
mod job;
mod layered;
mod resolve;

pub use activity::{
    V2ActivityCatalog, load_activity_catalog_asset, validate_catalog_activity_tools,
};
pub use error::{CatalogError, ResolveError, catalog_error_to_orbit};
pub use job::{V2JobCatalog, job_definition_files};
pub use resolve::{resolve_job_target_refs, resolve_job_target_refs_with_scripts};

use std::path::{Path, PathBuf};

#[cfg(doc)]
use orbit_types::workflow::{JobV2, JobV2StepBody, TargetRef};

/// `activity:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const ACTIVITY_REF_PREFIX: &str = "activity:";

/// `script:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const SCRIPT_REF_PREFIX: &str = orbit_types::workflow::activity_job::SCRIPT_REF_PREFIX;

/// `auto-task:<name>` prefix for the `target:` field on a [`TargetRef`].
pub const AUTO_TASK_REF_PREFIX: &str = orbit_types::workflow::activity_job::AUTO_TASK_REF_PREFIX;

/// A precedence-ordered, canonical-path-deduplicated catalog directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDirectory<K> {
    path: PathBuf,
    kind: K,
}

impl<K> CatalogDirectory<K> {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn kind(&self) -> &K {
        &self.kind
    }
}

/// Builds a catalog directory list while preserving the first occurrence of
/// each canonical path. Adapters push directories in their policy order.
#[derive(Debug, Clone)]
pub struct CatalogDirectoryList<K> {
    dirs: Vec<CatalogDirectory<K>>,
    seen: std::collections::BTreeSet<PathBuf>,
}

impl<K> Default for CatalogDirectoryList<K> {
    fn default() -> Self {
        Self {
            dirs: Vec::new(),
            seen: std::collections::BTreeSet::new(),
        }
    }
}

impl<K> CatalogDirectoryList<K> {
    pub fn push(&mut self, path: PathBuf, kind: K) {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if self.seen.insert(canonical) {
            self.dirs.push(CatalogDirectory { path, kind });
        }
    }

    pub fn into_vec(self) -> Vec<CatalogDirectory<K>> {
        self.dirs
    }
}
