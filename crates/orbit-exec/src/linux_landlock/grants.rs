//! Compiled Landlock path grants: what a rule lets the child do with one path,
//! the read and write predicates over a grant list, and de-duplication.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What a compiled grant lets the child do with one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LandlockGrant {
    /// Read, execute, and relocate anything beneath a directory.
    ReadTree,
    /// List a directory without reading the files directly inside it.
    ///
    /// Applied to a directory that holds a denied path: its allowed children
    /// are granted individually, so the denied one keeps no readable
    /// ancestor while the rest of the tree stays usable.
    ListOnly,
    /// Read and execute one file.
    ReadFile,
    /// Read, execute, and modify anything beneath a directory: create,
    /// remove, rename, truncate, write. Only a write-confining ruleset
    /// ([`LandlockBoundary`](super::LandlockBoundary)) hands out this grant.
    WriteTree,
    /// Read, write, and truncate one existing file.
    WriteFile,
}

/// One compiled Landlock rule: a path and what the child may do beneath it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPathGrant {
    pub path: PathBuf,
    pub grant: LandlockGrant,
}

impl LandlockPathGrant {
    pub(super) fn read_tree(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ReadTree,
        }
    }

    pub(super) fn list_only(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ListOnly,
        }
    }

    pub(super) fn read_file(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::ReadFile,
        }
    }

    pub(super) fn write_tree(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::WriteTree,
        }
    }

    pub(super) fn write_file(path: PathBuf) -> Self {
        Self {
            path,
            grant: LandlockGrant::WriteFile,
        }
    }

    /// Whether this grant alone lets the child open `path` for reading.
    pub fn reads(&self, path: &Path) -> bool {
        match self.grant {
            LandlockGrant::ReadTree | LandlockGrant::WriteTree => path.starts_with(&self.path),
            LandlockGrant::ReadFile | LandlockGrant::WriteFile => path == self.path,
            LandlockGrant::ListOnly => false,
        }
    }

    /// Whether this grant alone lets the child modify `path`.
    pub fn writes(&self, path: &Path) -> bool {
        match self.grant {
            LandlockGrant::WriteTree => path.starts_with(&self.path),
            LandlockGrant::WriteFile => path == self.path,
            LandlockGrant::ReadTree | LandlockGrant::ReadFile | LandlockGrant::ListOnly => false,
        }
    }
}

/// Whether any compiled grant lets the child read `path`.
pub fn grants_read(grants: &[LandlockPathGrant], path: &Path) -> bool {
    // Workspace grants are compiled from canonical paths. A query for a file
    // that has not been created yet cannot itself be canonicalized, so the
    // same existing-prefix resolution the grants were compiled under decides
    // its identity here.
    let path = crate::path_identity::physical_with_missing_tail(path);
    grants.iter().any(|grant| grant.reads(&path))
}

pub(super) fn dedupe(grants: Vec<LandlockPathGrant>) -> Vec<LandlockPathGrant> {
    let mut seen = BTreeSet::new();
    grants
        .into_iter()
        .filter(|grant| seen.insert((grant.path.clone(), grant.grant)))
        .collect()
}
